# Builds RiftReel for everyday use (release, its UI inside) and installs it for this
# Windows user in %LOCALAPPDATA%\Programs\RiftReel, with ffmpeg and its license beside
# it and a "RiftReel" Start menu shortcut carrying the app's id (Windows notifications
# need it, and a taskbar pin made from it groups with the window).
#
# Run it again after changes. Windows won't overwrite a program while it runs, but it
# will rename it: a running RiftReel (or a recording's ffmpeg) keeps going from the old
# file and the next start is the new build. Quit RiftReel from the tray and open it
# again to switch. -NoBuild installs what is already built.
#
# Needs the ffmpeg from tools/fetch-ffmpeg.ps1. On a machine whose newest MSVC toolset is
# broken, set VCToolsVersion first (see README).
param([switch]$NoBuild)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $env:LOCALAPPDATA 'Programs\RiftReel'
$config = Get-Content (Join-Path $repo 'src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json
$binaries = Join-Path $repo 'src-tauri\binaries'
$ffmpeg = Join-Path $binaries 'ffmpeg-x86_64-pc-windows-msvc.exe'
if (-not (Test-Path $ffmpeg)) { throw 'No ffmpeg: run tools/fetch-ffmpeg.ps1 first.' }
$built = Join-Path $repo 'target\release\riftreel.exe'

if (-not $NoBuild) {
    & npm --prefix (Join-Path $repo 'ui') run build
    if ($LASTEXITCODE) { throw 'The UI build failed.' }
    # At low priority: League may be running. (`start /wait` passes on cargo's exit code.)
    Push-Location (Join-Path $repo 'src-tauri')
    try {
        & cmd /c 'start "" /low /b /wait cargo build --release --features tauri/custom-protocol'
    } finally {
        Pop-Location
    }
    if ($LASTEXITCODE) { throw 'The build failed.' }
}
if (-not (Test-Path $built)) { throw "Nothing built at $built." }

New-Item -ItemType Directory -Force $dest | Out-Null
# What earlier runs set aside, once nothing runs from it any more.
Get-ChildItem $dest -Recurse -File -Filter '*.old-*' |
    ForEach-Object { Remove-Item $_.FullName -ErrorAction SilentlyContinue }

# Copies $from to $name in the install folder, unless it is there already; the file
# there (which may be running) is renamed out of the way first.
function Install-File([string]$from, [string]$name) {
    $to = Join-Path $dest $name
    if (Test-Path $to) {
        if ((Get-FileHash $to).Hash -eq (Get-FileHash $from).Hash) { return }
        $stem = [IO.Path]::GetFileNameWithoutExtension($to)
        $extension = [IO.Path]::GetExtension($to)
        Rename-Item $to "$stem.old-$(Get-Date -Format yyyyMMddHHmmssfff)$extension"
    }
    New-Item -ItemType Directory -Force (Split-Path -Parent $to) | Out-Null
    Copy-Item $from $to
}

Install-File $built 'riftreel.exe'
Install-File $ffmpeg 'ffmpeg.exe'
foreach ($file in 'LICENSE.txt', 'SOURCE.txt') {
    Install-File (Join-Path $binaries "ffmpeg-license\$file") "ffmpeg-license\$file"
}

# A shortcut with System.AppUserModel.ID set: WScript.Shell can't set it, so COM it is.
if (-not ('RiftReelShortcut' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;

public static class RiftReelShortcut
{
    [ComImport, Guid("00021401-0000-0000-C000-000000000046")]
    class ShellLink { }

    [ComImport, Guid("000214F9-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IShellLinkW
    {
        void GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder file, int max, IntPtr data, uint flags);
        void GetIDList(out IntPtr list);
        void SetIDList(IntPtr list);
        void GetDescription([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder name, int max);
        void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string name);
        void GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder dir, int max);
        void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string dir);
        void GetArguments([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder args, int max);
        void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string args);
        void GetHotkey(out short key);
        void SetHotkey(short key);
        void GetShowCmd(out int show);
        void SetShowCmd(int show);
        void GetIconLocation([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int max, out int index);
        void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string path, int index);
        void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string path, uint reserved);
        void Resolve(IntPtr window, uint flags);
        void SetPath([MarshalAs(UnmanagedType.LPWStr)] string file);
    }

    [StructLayout(LayoutKind.Sequential, Pack = 4)]
    struct PropertyKey
    {
        public Guid FormatId;
        public uint PropertyId;
    }

    [StructLayout(LayoutKind.Explicit)]
    struct PropVariant
    {
        [FieldOffset(0)] public ushort Type;
        [FieldOffset(8)] public IntPtr Value;
    }

    [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IPropertyStore
    {
        void GetCount(out uint count);
        void GetAt(uint index, out PropertyKey key);
        void GetValue(ref PropertyKey key, out PropVariant value);
        void SetValue(ref PropertyKey key, ref PropVariant value);
        void Commit();
    }

    [DllImport("ole32.dll")]
    static extern int PropVariantClear(ref PropVariant value);

    const ushort VT_LPWSTR = 31;

    // System.AppUserModel.ID
    static PropertyKey AppIdKey = new PropertyKey
    {
        FormatId = new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"),
        PropertyId = 5,
    };

    public static void Create(string shortcut, string target, string folder, string description, string appId)
    {
        var link = (IShellLinkW)new ShellLink();
        link.SetPath(target);
        link.SetWorkingDirectory(folder);
        link.SetDescription(description);
        link.SetIconLocation(target, 0);
        var value = new PropVariant { Type = VT_LPWSTR, Value = Marshal.StringToCoTaskMemUni(appId) };
        try
        {
            var store = (IPropertyStore)link;
            store.SetValue(ref AppIdKey, ref value);
            store.Commit();
        }
        finally
        {
            Marshal.FreeCoTaskMem(value.Value);
        }
        ((IPersistFile)link).Save(shortcut, true);
    }

    public static string AppIdOf(string shortcut)
    {
        var link = (IShellLinkW)new ShellLink();
        ((IPersistFile)link).Load(shortcut, 0);
        PropVariant value;
        ((IPropertyStore)link).GetValue(ref AppIdKey, out value);
        try
        {
            return value.Type == VT_LPWSTR ? Marshal.PtrToStringUni(value.Value) : null;
        }
        finally
        {
            PropVariantClear(ref value);
        }
    }
}
'@
}

$exe = Join-Path $dest 'riftreel.exe'
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'RiftReel.lnk'
[RiftReelShortcut]::Create($shortcut, $exe, $dest, $config.bundle.shortDescription, $config.identifier)

"Installed RiftReel $($config.version) in $dest"
"Start menu shortcut $shortcut (app id $([RiftReelShortcut]::AppIdOf($shortcut)))"
$running = Get-Process riftreel -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$dest\*" }
if ($running) { 'RiftReel is running: quit it from the tray and open it again for this build.' }
