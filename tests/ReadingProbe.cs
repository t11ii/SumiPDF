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
    public static void TabShortcut(IntPtr window, int key, bool shift, int expectedIndex, int expectedCount) {
        var tabs = GetDlgItem(window, 133);
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
            keys[0x11] = 0x80; // VK_CONTROL
            keys[0x10] = (byte)(shift ? 0x80 : 0);
            keys[0x12] = 0; // VK_MENU
            if (!SetKeyboardState(keys)) throw new Win32Exception(Marshal.GetLastWin32Error());
            Post(window, 0x0100, (uint)key, new IntPtr(1)); // WM_KEYDOWN into the ordinary shortcut handler
            Post(window, 0x0101, (uint)key, new IntPtr(unchecked((int)0xC0000001))); // WM_KEYUP
            var elapsed = Stopwatch.StartNew();
            while (elapsed.ElapsedMilliseconds < 5000) {
                if (Query(tabs, 0x1304) == expectedCount && Query(tabs, 0x130B) == expectedIndex) return;
                Thread.Sleep(15);
            }
            throw new InvalidOperationException("Tab shortcut did not select index " + expectedIndex +
                " of " + expectedCount + " tabs (index " + Query(tabs, 0x130B) + ", count " + Query(tabs, 0x1304) + ").");
        } finally {
            if (haveState) SetKeyboardState(saved);
            AttachThreadInput(self, viewer, false);
        }
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
    public static string Viewport(IntPtr parent) {
        var canvas = GetDlgItem(parent, 110);
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            var size = ClientSize(canvas);
            var screen = GetDC(canvas);
            if (screen == IntPtr.Zero) throw new InvalidOperationException("Canvas has no device context.");
            var dc = CreateCompatibleDC(screen);
            var bitmap = CreateCompatibleBitmap(screen, size[0], size[1]);
            var old = SelectObject(dc, bitmap);
            try { return Capture(canvas, dc, size[0], size[1]); }
            finally {
                SelectObject(dc, old); DeleteObject(bitmap); DeleteDC(dc); ReleaseDC(canvas, screen);
            }
        } finally { if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous); }
    }
}
