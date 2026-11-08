# Read and write .lnk files through the explicit Unicode Shell Link interface.
# WScript.Shell's automation TargetPath can round-trip a Unicode path through
# the active ANSI code page. These checks must inspect what Explorer launches.
if (-not ('EchoInstallerValidation.Shortcuts' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;

namespace EchoInstallerValidation {
    public sealed class ShortcutInfo {
        public string TargetPath { get; set; }
        public string WorkingDirectory { get; set; }
        public string Arguments { get; set; }
    }

    [ComImport, Guid("00021401-0000-0000-C000-000000000046")]
    internal class ShellLink { }

    [ComImport, Guid("000214F9-0000-0000-C000-000000000046"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    internal interface IShellLinkW {
        void GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int capacity, IntPtr findData, uint flags);
        void GetIDList(out IntPtr list);
        void SetIDList(IntPtr list);
        void GetDescription([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder description, int capacity);
        void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string description);
        void GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder directory, int capacity);
        void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string directory);
        void GetArguments([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder arguments, int capacity);
        void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string arguments);
        void GetHotkey(out short hotkey);
        void SetHotkey(short hotkey);
        void GetShowCmd(out int command);
        void SetShowCmd(int command);
        void GetIconLocation([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int capacity, out int index);
        void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string path, int index);
        void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string path, uint reserved);
        void Resolve(IntPtr window, uint flags);
        void SetPath([MarshalAs(UnmanagedType.LPWStr)] string path);
    }

    public static class Shortcuts {
        public static ShortcutInfo Read(string path) {
            object instance = new ShellLink();
            try {
                ((IPersistFile)instance).Load(path, 0);
                var link = (IShellLinkW)instance;
                var target = new StringBuilder(32768);
                var directory = new StringBuilder(32768);
                var arguments = new StringBuilder(32768);
                link.GetPath(target, target.Capacity, IntPtr.Zero, 4); // SLGP_RAWPATH: do not resolve or repair a stale link.
                link.GetWorkingDirectory(directory, directory.Capacity);
                link.GetArguments(arguments, arguments.Capacity);
                return new ShortcutInfo { TargetPath = target.ToString(), WorkingDirectory = directory.ToString(), Arguments = arguments.ToString() };
            } finally { Marshal.FinalReleaseComObject(instance); }
        }

        public static void Write(string path, string target, string directory, string arguments) {
            object instance = new ShellLink();
            try {
                var link = (IShellLinkW)instance;
                link.SetPath(target);
                link.SetWorkingDirectory(directory);
                link.SetArguments(arguments);
                ((IPersistFile)instance).Save(path, true);
            } finally { Marshal.FinalReleaseComObject(instance); }
        }
    }
}
'@
}
