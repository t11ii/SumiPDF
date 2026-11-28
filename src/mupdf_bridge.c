/* SPDX-License-Identifier: AGPL-3.0-only */
#include "mupdf/fitz.h"
#include "mupdf/pdf.h"

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <fcntl.h>
#include <io.h>
#endif

#define ERROR_SIZE 512
#define MAX_GLYPHS 100000

typedef struct {
    fz_context *ctx;
    FILE *file;
    pdf_document *doc;
    pdf_page *page;
    int page_index;
} sumi_pdf;

typedef struct {
    float left, top, right, bottom;
} sumi_rect;

typedef int (*sumi_glyph_fn)(void *, sumi_rect, int, int, int);

static void error_message(char *error, const char *message)
{
    snprintf(error, ERROR_SIZE, "%s", message);
}

void sumi_pdf_drop(sumi_pdf *pdf)
{
    if (!pdf)
        return;
    if (pdf->ctx) {
        pdf_drop_page(pdf->ctx, pdf->page);
        pdf_drop_document(pdf->ctx, pdf->doc);
        fz_drop_context(pdf->ctx);
    }
    fclose(pdf->file);
    free(pdf);
}

static sumi_pdf *open_pdf(FILE *file, int *count, char *error)
{
    sumi_pdf *pdf = calloc(1, sizeof(*pdf));
    if (!pdf) {
        fclose(file);
        error_message(error, "Could not allocate the PDF reader.");
        return NULL;
    }
    pdf->file = file;
    pdf->page_index = -1;
    fz_context *ctx = fz_new_context(NULL, NULL, 32u * 1024u * 1024u);
    pdf->ctx = ctx;
    if (!ctx) {
        error_message(error, "Could not initialize MuPDF.");
        sumi_pdf_drop(pdf);
        return NULL;
    }
    fz_set_error_callback(ctx, NULL, NULL);
    fz_set_warning_callback(ctx, NULL, NULL);
    fz_stream *stream = NULL;
    fz_var(stream);
    fz_try(ctx) {
        stream = fz_open_file_ptr_no_close(ctx, file);
        pdf->doc = pdf_open_document_with_stream(ctx, stream);
        if (pdf_needs_password(ctx, pdf->doc))
            fz_throw(ctx, FZ_ERROR_ARGUMENT, "This PDF requires a password.");
        *count = pdf_count_pages(ctx, pdf->doc);
        if (*count < 1)
            fz_throw(ctx, FZ_ERROR_FORMAT, "This PDF has no pages.");
    }
    fz_always(ctx) {
        fz_drop_stream(ctx, stream);
    }
    fz_catch(ctx) {
        error_message(error, fz_caught_message(ctx));
        sumi_pdf_drop(pdf);
        return NULL;
    }
    return pdf;
}

#ifdef _WIN32
sumi_pdf *sumi_pdf_open_handle(void *handle, int *count, char *error)
{
    int fd = _open_osfhandle((intptr_t)handle, _O_RDONLY | _O_BINARY);
    if (fd == -1) {
        CloseHandle(handle);
        error_message(error, "Could not allocate a descriptor for the PDF.");
        return NULL;
    }
    FILE *file = _fdopen(fd, "rb");
    if (!file) {
        _close(fd);
        error_message(error, "Could not create a stream for the PDF.");
        return NULL;
    }
    return open_pdf(file, count, error);
}
#else
/* The non-Windows build exercises the engine through the test fixtures. */
sumi_pdf *sumi_pdf_open_path(const char *path, int *count, char *error)
{
    FILE *file = fopen(path, "rb");
    if (!file) {
        error_message(error, "Could not open the PDF.");
        return NULL;
    }
    return open_pdf(file, count, error);
}
#endif

/* Called inside an exception handler owned by the public entry point. */
static fz_rect load_page(sumi_pdf *pdf, int index)
{
    fz_context *ctx = pdf->ctx;
    if (index != pdf->page_index) {
        pdf_page *page = pdf_load_page(ctx, pdf->doc, index);
        pdf_drop_page(ctx, pdf->page);
        pdf->page = page;
        pdf->page_index = index;
    }
    fz_rect bounds = pdf_bound_page(ctx, pdf->page, FZ_CROP_BOX);
    if (!isfinite(bounds.x0) || !isfinite(bounds.y0) || !isfinite(bounds.x1) ||
        !isfinite(bounds.y1) || bounds.x1 <= bounds.x0 || bounds.y1 <= bounds.y0)
        fz_throw(ctx, FZ_ERROR_FORMAT, "Invalid PDF page dimensions.");
    return bounds;
}

int sumi_pdf_ratio(sumi_pdf *pdf, int index, double *ratio, char *error)
{
    fz_context *ctx = pdf->ctx;
    fz_try(ctx) {
        fz_rect bounds = load_page(pdf, index);
        *ratio = ((double)bounds.x1 - bounds.x0) / ((double)bounds.y1 - bounds.y0);
    }
    fz_catch(ctx) {
        error_message(error, fz_caught_message(ctx));
        return 0;
    }
    return 1;
}

int sumi_pdf_render(sumi_pdf *pdf, int index, int width, int height,
    unsigned char *bgra, char *error)
{
    fz_context *ctx = pdf->ctx;
    fz_pixmap *pixmap = NULL;
    fz_device *device = NULL;
    fz_var(pixmap);
    fz_var(device);
    fz_try(ctx) {
        fz_rect bounds = load_page(pdf, index);
        float sx = width / (bounds.x1 - bounds.x0);
        float sy = height / (bounds.y1 - bounds.y0);
        fz_matrix transform = {sx, 0, 0, sy, -bounds.x0 * sx, -bounds.y0 * sy};
        pixmap = fz_new_pixmap_with_data(ctx, fz_device_bgr(ctx),
            width, height, NULL, 1, width * 4, bgra);
        device = fz_new_draw_device(ctx, transform, pixmap);
        pdf_run_page(ctx, pdf->page, device, fz_identity, NULL);
        fz_close_device(ctx, device);
    }
    fz_always(ctx) {
        fz_drop_device(ctx, device);
        fz_drop_pixmap(ctx, pixmap);
    }
    fz_catch(ctx) {
        error_message(error, fz_caught_message(ctx));
        return 0;
    }
    return 1;
}

int sumi_pdf_text(sumi_pdf *pdf, int index, void *output, sumi_glyph_fn glyph, char *error)
{
    fz_context *ctx = pdf->ctx;
    fz_stext_page *text = NULL;
    fz_var(text);
    fz_try(ctx) {
        fz_rect bounds = load_page(pdf, index);
        float width = bounds.x1 - bounds.x0;
        float height = bounds.y1 - bounds.y0;
        fz_stext_options options = {0};
        options.flags = FZ_STEXT_CLIP;
        text = fz_new_stext_page_from_page(ctx, (fz_page *)pdf->page, &options);
        int count = 0, next_line = 0;
        for (fz_stext_block *block = text->first_block; block; block = block->next) {
            if (block->type != FZ_STEXT_BLOCK_TEXT)
                continue;
            for (fz_stext_line *line = block->u.t.first_line; line; line = line->next) {
                int direction = fabsf(line->dir.x) >= fabsf(line->dir.y)
                    ? (line->dir.x >= 0 ? 0 : 2) : (line->dir.y >= 0 ? 1 : 3);
                int line_index = next_line++;
                for (fz_stext_char *ch = line->first_char; ch; ch = ch->next) {
                    fz_rect rect = fz_rect_from_quad(ch->quad);
                    sumi_rect normalized = {
                        (rect.x0 - bounds.x0) / width, (rect.y0 - bounds.y0) / height,
                        (rect.x1 - bounds.x0) / width, (rect.y1 - bounds.y0) / height,
                    };
                    int unicode = ch->flags & (FZ_STEXT_UNICODE_IS_CID | FZ_STEXT_UNICODE_IS_GID)
                        ? 0 : ch->c;
                    if (++count > MAX_GLYPHS || !glyph(output, normalized, unicode, line_index, direction))
                        fz_throw(ctx, FZ_ERROR_LIMIT, "This page has too much text to select.");
                }
            }
        }
    }
    fz_always(ctx) {
        fz_drop_stext_page(ctx, text);
    }
    fz_catch(ctx) {
        error_message(error, fz_caught_message(ctx));
        return 0;
    }
    return 1;
}
