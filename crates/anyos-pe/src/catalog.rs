//! Classification of imported DLL names into Windows API families.
//!
//! Used by compatibility reports to say *which part* of the runtime an import
//! needs, and to tell Windows system DLLs (which AnyOS must provide) apart from
//! application DLLs (which must ship next to the executable).

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum ApiFamily {
    /// kernel32 / kernelbase and core API sets: processes, memory, files, threads.
    Kernel,
    /// ntdll: native NT system services.
    Ntdll,
    /// C/C++ runtime: msvcrt, ucrt, vcruntime, msvcp.
    Crt,
    /// user32: windows, messages, input.
    User,
    /// gdi32 and friends: 2D drawing and text.
    Gdi,
    /// advapi32 and security/registry helpers.
    Advapi,
    /// Shell and common controls: shell32, shlwapi, comctl32, comdlg32.
    Shell,
    /// COM / OLE / RPC.
    Com,
    /// Sockets, HTTP, name resolution.
    Networking,
    /// Direct3D, DXGI, DirectDraw, OpenGL, Vulkan.
    Graphics3d,
    /// winmm, DirectSound, XAudio2, Core Audio.
    Audio,
    /// DirectInput, XInput, raw HID.
    Input,
    /// CNG, CryptoAPI, SSPI.
    Crypto,
    /// mscoree: .NET runtime bootstrap.
    DotNet,
    /// Other Windows system DLLs (version, psapi, dbghelp, setupapi, ...).
    SystemOther,
    /// Kernel-mode only (ntoskrnl, hal): never satisfiable in user mode.
    KernelMode,
    /// Not a known system DLL: expected to ship with the application.
    Application,
}

impl ApiFamily {
    pub fn display_name(self) -> &'static str {
        match self {
            ApiFamily::Kernel => "Kernel (kernel32)",
            ApiFamily::Ntdll => "NT native (ntdll)",
            ApiFamily::Crt => "C/C++ runtime",
            ApiFamily::User => "Windowing & input (user32)",
            ApiFamily::Gdi => "2D graphics (gdi32)",
            ApiFamily::Advapi => "Registry & security (advapi32)",
            ApiFamily::Shell => "Shell & common controls",
            ApiFamily::Com => "COM / OLE / RPC",
            ApiFamily::Networking => "Networking",
            ApiFamily::Graphics3d => "3D graphics (Direct3D/OpenGL/Vulkan)",
            ApiFamily::Audio => "Audio",
            ApiFamily::Input => "Game input",
            ApiFamily::Crypto => "Cryptography",
            ApiFamily::DotNet => ".NET bootstrap (mscoree)",
            ApiFamily::SystemOther => "Other system DLLs",
            ApiFamily::KernelMode => "Kernel-mode",
            ApiFamily::Application => "Application DLLs",
        }
    }

    pub fn is_system(self) -> bool {
        self != ApiFamily::Application
    }
}

/// Classify a DLL name as written in an import table (case-insensitive).
pub fn classify(dll: &str) -> ApiFamily {
    let name = dll.to_ascii_lowercase();
    let stem = name.strip_suffix(".dll").unwrap_or(&name);

    // API set contracts: api-ms-win-<area>-..., ext-ms-win-<area>-...
    if let Some(rest) = stem
        .strip_prefix("api-ms-win-")
        .or_else(|| stem.strip_prefix("ext-ms-win-"))
    {
        return classify_api_set(rest);
    }

    match stem {
        "kernel32" | "kernelbase" => ApiFamily::Kernel,
        "ntdll" => ApiFamily::Ntdll,
        "user32" | "imm32" | "win32u" => ApiFamily::User,
        "gdi32" | "gdi32full" | "gdiplus" | "msimg32" | "usp10" | "d2d1" | "dwrite"
        | "windowscodecs" => ApiFamily::Gdi,
        "advapi32" | "sechost" | "userenv" | "wtsapi32" | "netapi32" => ApiFamily::Advapi,
        "shell32" | "shlwapi" | "comctl32" | "comdlg32" | "uxtheme" | "dwmapi" | "propsys"
        | "shcore" => ApiFamily::Shell,
        "ole32" | "oleaut32" | "olepro32" | "combase" | "rpcrt4" | "oleacc" | "urlmon" => {
            ApiFamily::Com
        }
        "ws2_32" | "wsock32" | "mswsock" | "winhttp" | "wininet" | "iphlpapi" | "dnsapi"
        | "netutils" | "websocket" | "httpapi" => ApiFamily::Networking,
        "d3d9" | "d3d10" | "d3d10_1" | "d3d11" | "d3d12" | "dxgi" | "ddraw" | "opengl32"
        | "glu32" | "vulkan-1" | "d3d8" | "dxcore" => ApiFamily::Graphics3d,
        "winmm" | "dsound" | "mmdevapi" | "avrt" | "msacm32" | "audioses" | "mfplat" | "mf"
        | "mfreadwrite" => ApiFamily::Audio,
        "dinput" | "dinput8" | "hid" | "xinput9_1_0" => ApiFamily::Input,
        "bcrypt" | "bcryptprimitives" | "crypt32" | "ncrypt" | "secur32" | "sspicli"
        | "wintrust" => ApiFamily::Crypto,
        "mscoree" => ApiFamily::DotNet,
        "ntoskrnl" | "ntoskrnl.exe" | "hal" | "ndis" | "wdfldr" | "fltmgr" => ApiFamily::KernelMode,
        "msvcrt" | "ucrtbase" | "ucrtbased" | "crtdll" | "concrt140" => ApiFamily::Crt,
        "version" | "psapi" | "dbghelp" | "dbgcore" | "setupapi" | "cfgmgr32" | "powrprof"
        | "winspool.drv" | "winspool" | "wtsapi" | "wevtapi" | "pdh" | "credui" | "msi"
        | "wldap32" | "normaliz" | "profapi" | "devobj" | "winsta" | "mpr" | "apphelp"
        | "srvcli" | "cabinet" | "msvfw32" | "avifil32" | "tdh" | "wer" | "virtdisk"
        | "wintypes" | "coremessaging" => ApiFamily::SystemOther,
        _ if is_versioned(
            stem,
            &[
                "msvcr",
                "msvcp",
                "vcruntime",
                "vcomp",
                "vccorlib",
                "mfc",
                "atl",
            ],
        ) =>
        {
            ApiFamily::Crt
        }
        _ if is_versioned(stem, &["d3dx9_", "d3dx10_", "d3dx11_", "d3dcompiler_"]) => {
            ApiFamily::Graphics3d
        }
        _ if is_versioned(stem, &["xaudio2_", "x3daudio1_", "xapofx1_"]) => ApiFamily::Audio,
        _ if is_versioned(stem, &["xinput1_"]) => ApiFamily::Input,
        _ => ApiFamily::Application,
    }
}

/// Matches names like `msvcp140`, `msvcp140_1`, `vcruntime140_1`, `msvcr120_app`.
fn is_versioned(stem: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| {
        stem.strip_prefix(p)
            .is_some_and(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
    })
}

fn classify_api_set(rest: &str) -> ApiFamily {
    let area = rest.split('-').next().unwrap_or("");
    match area {
        "crt" => ApiFamily::Crt,
        "core" | "eventing" | "power" | "devices" | "appmodel" | "kernel32" => ApiFamily::Kernel,
        "ntuser" | "rtcore" | "input" => ApiFamily::User,
        "gdi" | "dx" => ApiFamily::Gdi,
        "security" | "service" | "advapi32" => ApiFamily::Advapi,
        "shell" | "shcore" | "shlwapi" | "uxtheme" => ApiFamily::Shell,
        "ole32" | "oleaut32" | "com" | "rpc" => ApiFamily::Com,
        "net" | "winsock" | "networking" => ApiFamily::Networking,
        "mm" | "audio" => ApiFamily::Audio,
        _ => ApiFamily::SystemOther,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_dlls() {
        assert_eq!(classify("KERNEL32.dll"), ApiFamily::Kernel);
        assert_eq!(classify("ntdll.dll"), ApiFamily::Ntdll);
        assert_eq!(
            classify("api-ms-win-crt-runtime-l1-1-0.dll"),
            ApiFamily::Crt
        );
        assert_eq!(
            classify("api-ms-win-core-synch-l1-2-0.dll"),
            ApiFamily::Kernel
        );
        assert_eq!(classify("VCRUNTIME140_1.dll"), ApiFamily::Crt);
        assert_eq!(classify("MSVCP140.dll"), ApiFamily::Crt);
        assert_eq!(classify("d3d11.dll"), ApiFamily::Graphics3d);
        assert_eq!(classify("d3dcompiler_47.dll"), ApiFamily::Graphics3d);
        assert_eq!(classify("XINPUT1_4.dll"), ApiFamily::Input);
        assert_eq!(classify("mscoree.dll"), ApiFamily::DotNet);
        assert_eq!(classify("ntoskrnl.exe"), ApiFamily::KernelMode);
        assert_eq!(classify("exports_dll.dll"), ApiFamily::Application);
        assert_eq!(classify("msvcrtx.dll"), ApiFamily::Application);
    }
}
