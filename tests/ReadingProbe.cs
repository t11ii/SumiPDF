using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Threading;

// Native input and observations for the Windows release EXE, including runners
// without a foreground desktop. The PDF fixture has distinct page colors.
public static class ReadingProbe {
    [DllImport("user32.dll")] static extern IntPtr GetDlgItem(IntPtr window, int id);
    [DllImport("user32.dll", SetLastError=true)]
    static extern bool PostMessageW(IntPtr window, uint msg, UIntPtr wp, IntPtr lp);
    [DllImport("user32.dll", SetLastError=true)]
    static extern IntPtr SendMessageTimeoutW(IntPtr window, uint msg, UIntPtr wp, IntPtr lp,
        uint flags, uint timeout, out UIntPtr result);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, IntPtr unused);
    [DllImport("user32.dll")] static extern uint GetQueueStatus(uint flags);
    [DllImport("user32.dll", SetLastError=true)] static extern bool AttachThreadInput(uint from, uint to, bool attach);
    [DllImport("user32.dll", SetLastError=true)] static extern bool GetKeyboardState([Out] byte[] keys);
    [DllImport("user32.dll", SetLastError=true)] static extern bool SetKeyboardState(byte[] keys);
    [StructLayout(LayoutKind.Sequential)] struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] static extern bool ScreenToClient(IntPtr window, ref Point point);
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr window, uint command);
    [DllImport("user32.dll")] static extern bool IsWindow(IntPtr window);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
    static extern IntPtr FindWindowExW(IntPtr parent, IntPtr after, string kind, string name);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", ExactSpelling=true, CharSet=CharSet.Unicode)]
    static extern IntPtr FindListEntry(IntPtr list, uint message, UIntPtr startAfter, string value,
        uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll")] static extern IntPtr GetKeyboardLayout(uint id);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
    static extern short VkKeyScanExW(char character, IntPtr layout);

    static void Post(IntPtr window, uint message, uint key, IntPtr position) {
        if (!PostMessageW(window, message, new UIntPtr(key), position))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not post viewer input.");
    }
    static int Query(IntPtr window, uint message) {
        UIntPtr result;
        if (SendMessageTimeoutW(window, message, UIntPtr.Zero, IntPtr.Zero, 2, 2000, out result) == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Tab control did not respond.");
        return unchecked((int)result.ToUInt64());
    }
    public static void Wheel(IntPtr window, int delta) {
        Post(GetDlgItem(window, 110), 0x020A, unchecked((uint)(delta << 16)), IntPtr.Zero);
    }
    public static void Command(IntPtr window, int id) {
        Post(window, 0x0111, (uint)id, IntPtr.Zero);
    }
    static void Shortcut(IntPtr window, int key, int modifiers, Func<bool> observed, string description) {
        uint self = GetCurrentThreadId(), viewer = GetWindowThreadProcessId(window, IntPtr.Zero);
        GetQueueStatus(0x04FF); // Create the caller's GUI queue before attaching.
        if (!AttachThreadInput(self, viewer, true))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not attach the viewer input queue.");
        var saved = new byte[256];
        bool haveState = false;
        try {
            haveState = GetKeyboardState(saved);
            if (!haveState) throw new Win32Exception(Marshal.GetLastWin32Error());
            var keys = (byte[])saved.Clone();
            keys[0x10] = (byte)((modifiers & 1) != 0 ? 0x80 : 0); // VK_SHIFT
            keys[0x11] = (byte)((modifiers & 2) != 0 ? 0x80 : 0); // VK_CONTROL
            keys[0x12] = (byte)((modifiers & 4) != 0 ? 0x80 : 0); // VK_MENU
            if (!SetKeyboardState(keys)) throw new Win32Exception(Marshal.GetLastWin32Error());
            Post(window, 0x0100, (uint)key, new IntPtr(1)); // WM_KEYDOWN into the ordinary shortcut handler
            Post(window, 0x0101, (uint)key, new IntPtr(unchecked((int)0xC0000001))); // WM_KEYUP
            var elapsed = Stopwatch.StartNew();
            while (elapsed.ElapsedMilliseconds < 5000) {
                if (observed()) return;
                Thread.Sleep(15);
            }
            throw new InvalidOperationException("Shortcut did not " + description + ".");
        } finally {
            if (haveState) SetKeyboardState(saved);
            AttachThreadInput(self, viewer, false);
        }
    }

    public static void TabShortcut(IntPtr window, int key, bool shift, int expectedIndex, int expectedCount) {
        var tabs = GetDlgItem(window, 133);
        Shortcut(window, key, shift ? 3 : 2,
            () => Query(tabs, 0x1304) == expectedCount && Query(tabs, 0x130B) == expectedIndex,
            "select tab " + expectedIndex + " of " + expectedCount);
    }
    public static void CheckShortcutHelp(IntPtr window) {
        short mapping = VkKeyScanExW('?', GetKeyboardLayout(GetWindowThreadProcessId(window, IntPtr.Zero)));
        if (mapping == -1) throw new InvalidOperationException("No question mark key on this test layout.");
        IntPtr dialog = IntPtr.Zero;
        Shortcut(window, mapping & 255, mapping >> 8, () => {
            dialog = IntPtr.Zero;
            while ((dialog = FindWindowExW(IntPtr.Zero, dialog, "#32770", "Keyboard shortcuts")) != IntPtr.Zero)
                if (GetWindow(dialog, 4) == window) return true; // GW_OWNER
            return false;
        }, "open keyboard help");
        var list = GetDlgItem(dialog, 1000);
        if (Query(list, 0x018B) < 19) throw new InvalidOperationException("Shortcut list is incomplete.");
        foreach (string item in new[] {"Ctrl+W\tClose the current tab", "H\tToggle highlight", "Ctrl+S\tSign PDF / save while signing"}) {
            UIntPtr index;
            if (FindListEntry(list, 0x01A2, UIntPtr.MaxValue, item, 2, 2000, out index) == IntPtr.Zero ||
                index.ToUInt64() > int.MaxValue) throw new InvalidOperationException("Missing shortcut: " + item);
        }
        Shortcut(list, 0x1B, 0, () => !IsWindow(dialog), "dismiss help with Escape");
    }
    public static void OpenFind(IntPtr window) {
        Shortcut(window, 0x46, 2, () => IsWindowVisible(GetDlgItem(window, 161)), "open Find");
    }
    public static void CloseFind(IntPtr window) {
        var edit = GetDlgItem(window, 161);
        Shortcut(edit, 0x1B, 0, () => !IsWindowVisible(edit), "close Find with Escape");
    }
    public static void SetFindQuery(IntPtr window, string text) {
        UIntPtr result;
        if (FindListEntry(GetDlgItem(window, 161), 0x000C, UIntPtr.Zero, text, 2, 2000, out result) == IntPtr.Zero
            || result == UIntPtr.Zero) throw new InvalidOperationException("Could not set the search text.");
    }
    public static void AppendFindQuestionMark(IntPtr window) {
        var edit = GetDlgItem(window, 161);
        int len = Query(edit, 0x000E); // WM_GETTEXTLENGTH
        UIntPtr ignored;
        SendMessageTimeoutW(edit, 0x00B1, new UIntPtr((uint)len), new IntPtr(len), 2, 2000, out ignored); // EM_SETSEL
        short key = VkKeyScanExW('?', GetKeyboardLayout(GetWindowThreadProcessId(edit, IntPtr.Zero)));
        if (key == -1) throw new InvalidOperationException("No question mark key on this test layout.");
        Shortcut(edit, key & 255, key >> 8, () => Query(edit, 0x000E) == len + 1, "type ? in Find");
    }
    public static void ClickClose(IntPtr window, string name) {
        var close = FindWindowExW(GetDlgItem(window, 133), IntPtr.Zero, "Button", "Close " + name);
        Rect bounds;
        if (close == IntPtr.Zero || !GetClientRect(close, out bounds) || bounds.Right < 1 || bounds.Bottom < 1)
            throw new InvalidOperationException("Missing close button for " + name);
        var point = new IntPtr(((bounds.Bottom / 2) << 16) | (bounds.Right / 2));
        Post(close, 0x0201, 1, point);
        Post(close, 0x0202, 0, point);
    }
    public static int TabCount(IntPtr window) {
        return Query(GetDlgItem(window, 133), 0x1304);
    }

    [ComImport, Guid("618736E0-3C3D-11CF-810C-00AA00389B71"), InterfaceType(ComInterfaceType.InterfaceIsIDispatch)]
    interface AccessibleTabs {
        [DispId(-5003)] string this[[MarshalAs(UnmanagedType.Struct)] object child] { get; }
        [DispId(-5015)] void accLocation(out int left, out int top, out int width, out int height,
            [MarshalAs(UnmanagedType.Struct)] object child);
    }
    [DllImport("oleacc.dll")]
    static extern int AccessibleObjectFromWindow(IntPtr window, uint id, ref Guid iid,
        [MarshalAs(UnmanagedType.Interface)] out AccessibleTabs accessible);
    public static string ControlName(IntPtr window, int id) {
        var iid = typeof(AccessibleTabs).GUID;
        AccessibleTabs accessible;
        Marshal.ThrowExceptionForHR(AccessibleObjectFromWindow(GetDlgItem(window, id), 0xFFFFFFFC, ref iid, out accessible));
        return accessible[0]; // CHILDID_SELF
    }
    public static void ClickTab(IntPtr window, string name) {
        var tabs = GetDlgItem(window, 133);
        var iid = typeof(AccessibleTabs).GUID;
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            AccessibleTabs items;
            Marshal.ThrowExceptionForHR(AccessibleObjectFromWindow(tabs, 0xFFFFFFFC, ref iid, out items));
            for (int child = 1; child <= Query(tabs, 0x1304); child++) {
                if (!(items[child] ?? "").Contains(name)) continue;
                int x, y, width, height;
                items.accLocation(out x, out y, out width, out height, child);
                var pt = new Point { X = x + width / 2, Y = y + height / 2 };
                if (width <= 0 || height <= 0 || !ScreenToClient(tabs, ref pt))
                    throw new InvalidOperationException("No visible tab bounds for " + name);
                var pos = new IntPtr((pt.Y << 16) | (pt.X & 0xffff));
                Post(tabs, 0x0201, 1, pos); // WM_LBUTTONDOWN
                Post(tabs, 0x0202, 0, pos); // WM_LBUTTONUP
                return;
            }
            throw new InvalidOperationException("No accessible tab named " + name);
        } finally { if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous); }
    }

    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
    [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr window);
    [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr window, IntPtr dc);
    [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleDC(IntPtr dc);
    [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleBitmap(IntPtr dc, int width, int height);
    [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr dc, IntPtr obj);
    [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr obj);
    [DllImport("gdi32.dll")] static extern bool DeleteDC(IntPtr dc);
    [DllImport("gdi32.dll")] static extern uint GetPixel(IntPtr dc, int x, int y);
    [DllImport("gdi32.dll")] static extern bool BitBlt(IntPtr dest, int x, int y, int w, int h,
        IntPtr src, int sx, int sy, uint operation);
    static char PageColor(uint pixel) {
        // PDF DeviceRGB, in the same order as Write-ReadingPdf. Allow renderer color rounding.
        int r = (int)(pixel & 255), g = (int)((pixel >> 8) & 255), b = (int)((pixel >> 16) & 255);
        if (Near(r, 199) && Near(g, 221) && Near(b, 242)) return 'B';
        if (Near(r, 245) && Near(g, 225) && Near(b, 199)) return 'O';
        if (Near(r, 210) && Near(g, 235) && Near(b, 212)) return 'G';
        if (Near(r, 231) && Near(g, 215) && Near(b, 242)) return 'V';
        if (Near(r, 235) && Near(g, 190) && Near(b, 213)) return 'R';
        return '.';
    }
    static bool Near(int a, int b) { return Math.Abs(a - b) <= 7; }
    static string Stripe(IntPtr dc, int width, int height) {
        var stripe = new char[(height + 3) / 4];
        for (int i = 0; i < stripe.Length; i++) stripe[i] = PageColor(GetPixel(dc, width / 2, i * 4));
        return new string(stripe);
    }
    public static int[] ClientSize(IntPtr window) {
        Rect r;
        if (!GetClientRect(window, out r) || r.Right <= 0 || r.Bottom <= 0)
            throw new InvalidOperationException("Missing viewer client area.");
        return new[] {r.Right, r.Bottom};
    }
    public static string Capture(IntPtr window, IntPtr target, int width, int height) {
        // Prefer the window's own paint path. Some Windows display drivers support
        // copying an already displayed client surface but reject PrintWindow.
        if (PrintWindow(window, target, 1)) {
            var result = Stripe(target, width, height);
            if (result.Trim('.').Length != 0) return result;
        }
        UIntPtr ignored;
        if (SendMessageTimeoutW(window, 0x0318, new UIntPtr(unchecked((ulong)target.ToInt64())),
            new IntPtr(4), 2, 1500, out ignored) != IntPtr.Zero) { // WM_PRINTCLIENT / PRF_CLIENT
            var result = Stripe(target, width, height);
            if (result.Trim('.').Length != 0) return result;
        }
        var src = GetDC(window);
        try {
            if (src != IntPtr.Zero && BitBlt(target, 0, 0, width, height, src, 0, 0, 0x00CC0020))
                return Stripe(target, width, height);
            return "";
        } finally { if (src != IntPtr.Zero) ReleaseDC(window, src); }
    }
    static T ReadCanvas<T>(IntPtr parent, Func<IntPtr, int, int, string, T> observe) {
        var canvas = GetDlgItem(parent, 110);
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            var size = ClientSize(canvas);
            var screen = GetDC(canvas);
            if (screen == IntPtr.Zero) throw new InvalidOperationException("Canvas has no device context.");
            var dc = CreateCompatibleDC(screen);
            var bitmap = CreateCompatibleBitmap(screen, size[0], size[1]);
            var old = SelectObject(dc, bitmap);
            try {
                string stripe = Capture(canvas, dc, size[0], size[1]);
                return observe(dc, size[0], size[1], stripe);
            }
            finally {
                SelectObject(dc, old); DeleteObject(bitmap); DeleteDC(dc); ReleaseDC(canvas, screen);
            }
        } finally { if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous); }
    }
    public static string Viewport(IntPtr parent) {
        return ReadCanvas(parent, (dc, w, h, stripe) => stripe);
    }
    public static int SearchHighlightPixels(IntPtr parent) {
        return ReadCanvas(parent, (dc, w, h, stripe) => {
            // Both matches are on blue pages. Recover the painted page bounds from
            // its untouched centerline and sample the area around the fixture's text.
            var blue = System.Text.RegularExpressions.Regex.Match(stripe, "B{12,}");
            if (!blue.Success) return -1;
            int top = blue.Index * 4, row = top + blue.Length * 2;
            int left = 0, right = w - 1;
            while (left < right && PageColor(GetPixel(dc, left, row)) != 'B') left++;
            while (right > left && PageColor(GetPixel(dc, right, row)) != 'B') right--;
            if (left == right) return -1;
            double scale = (right - left + 1) / 612.0;
            // Text starts at x=122 pt, baseline 347 pt from the top, size 24 pt.
            int x1 = left + (int)(110 * scale), x2 = left + (int)(299 * scale);
            int y1 = top + (int)(308 * scale), y2 = Math.Min(h - 1, top + (int)(365 * scale));
            int count = 0;
            for (int y = y1; y <= y2; y += 3) for (int x = x1; x <= x2; x += 3) {
                uint p = GetPixel(dc, x, y);
                int r = (int)(p & 255), g = (int)((p >> 8) & 255), b = (int)((p >> 16) & 255);
                if (r > 185 && r - g > 32 && g - b > 32) count++;
            }
            return count;
        });
    }
}
