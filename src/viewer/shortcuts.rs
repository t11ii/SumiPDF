use super::*;

const DIALOG: usize = 204;
const LIST: i32 = 1000;
const SYSTEM_HELP: usize = 0x1ff0;

// In display order. The native list uses a tab stop between the keys and action.
const BINDINGS: &[&str] = &[
    "Ctrl+O\tOpen PDFs in tabs",
    "Ctrl+Tab\tNext tab",
    "Ctrl+Shift+Tab\tPrevious tab",
    "Ctrl+W\tClose the current tab",
    "Left / Page Up\tPrevious page",
    "Right / Page Down\tNext page",
    "Home / End\tFirst / last page",
    "Ctrl+G, number, Enter\tGo to a page",
    "Ctrl+F\tFind text in the current PDF",
    "Enter / F3\tNext match",
    "Shift+Enter / Shift+F3\tPrevious match",
    "Ctrl+Plus / Ctrl+Minus\tZoom in / out",
    "Ctrl+0\tFit page",
    "Ctrl+wheel\tZoom in / out",
    "Shift+wheel\tScroll horizontally",
    "Drag text, then H / U\tHighlight or underline selected text",
    "H\tToggle highlight",
    "U\tToggle underline",
    "Ctrl+Z\tUndo the last annotation",
    "Ctrl+Shift+S\tSave an annotated copy",
    "Ctrl+S\tSign PDF / save while signing",
    "Esc\tLeave the current tool or dialog",
    "Tab / Shift+Tab\tFocus the next / previous control",
    "? (Shift+/)\tShow keyboard shortcuts",
];

pub(super) fn add_to_window_menu(hwnd: HWND, restore_tabs: bool) {
    unsafe {
        let menu = GetSystemMenu(hwnd, 0);
        if !menu.is_null() {
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            AppendMenuW(
                menu,
                MF_STRING | restore_tabs_flags(restore_tabs),
                RESTORE_TABS,
                wide("Restore previous tabs at startup").as_ptr(),
            );
            AppendMenuW(
                menu,
                MF_STRING,
                FORGET_CERTIFICATE,
                wide("Forget saved signing certificate").as_ptr(),
            );
            AppendMenuW(
                menu,
                MF_STRING,
                SYSTEM_HELP,
                wide("Keyboard shortcuts…\t?").as_ptr(),
            );
        }
    }
}

fn restore_tabs_flags(enabled: bool) -> u32 {
    if enabled { MF_CHECKED } else { MF_UNCHECKED }
}

pub(super) fn check_restore_tabs(hwnd: HWND, enabled: bool) {
    unsafe {
        let menu = GetSystemMenu(hwnd, 0);
        if !menu.is_null() {
            CheckMenuItem(menu, RESTORE_TABS as u32, restore_tabs_flags(enabled));
        }
    }
}

pub(super) fn is_menu_command(command: WPARAM) -> bool {
    command & 0xfff0 == SYSTEM_HELP
}

pub(super) fn show(owner: HWND) -> Result<(), String> {
    let result = unsafe {
        DialogBoxParamW(
            GetModuleHandleW(null()),
            DIALOG as *const u16,
            owner,
            Some(procedure),
            0,
        )
    };
    if result < 0 {
        Err("Could not open the keyboard shortcuts list.".into())
    } else {
        Ok(())
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, _: LPARAM) -> isize {
    unsafe {
        match msg {
            WM_INITDIALOG => {
                let list = GetDlgItem(hwnd, LIST);
                let tab_stop: i32 = 116; // Dialog units from the left edge of the list.
                SendMessageW(list, LB_SETTABSTOPS, 1, (&raw const tab_stop) as LPARAM);
                for &row in BINDINGS {
                    if SendMessageW(list, LB_ADDSTRING, 0, wide(row).as_ptr() as LPARAM) < 0 {
                        EndDialog(hwnd, -1);
                        return 1;
                    }
                }
                SendMessageW(list, LB_SETCURSEL, 0, 0);
                let _ = theme::style_control(GetDlgItem(hwnd, IDOK));
                theme::refresh(hwnd);
                1
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
                theme::refresh(hwnd);
                0
            }
            WM_CTLCOLORDLG | WM_CTLCOLORSTATIC | WM_CTLCOLORLISTBOX
                if !theme::palette().high_contrast =>
            {
                theme::control_color(w as HDC, msg == WM_CTLCOLORLISTBOX, true)
            }
            WM_COMMAND if matches!((w & 0xffff) as i32, IDOK | IDCANCEL) => {
                EndDialog(hwnd, 0);
                1
            }
            WM_CLOSE => {
                EndDialog(hwnd, 0);
                1
            }
            _ => 0,
        }
    }
}
