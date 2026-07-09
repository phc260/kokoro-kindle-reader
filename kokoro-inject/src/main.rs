//! Minimal LoadLibrary injector for the Kokoro hook. `kokoro-host` spawns this (fire and
//! forget) when it sees Kindle start; it loads `kokoro_hook.dll` into Kindle, which patches
//! `ISpVoice::SetVoice` so Kindle narrates with Kokoro.
//!
//!   kokoro-inject <path\to\kokoro_hook.dll>
//!
//! Same-bitness only (x86 injector -> x86 Kindle) so the injector's own kernel32!LoadLibraryW
//! address is valid in the target. Windowless in release (spawned by the windowless host, so
//! no console to write to) — outcomes go to %TEMP%\kokoro-inject.log; nothing here panics into
//! a Windows error dialog.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::c_void;
use std::mem::{size_of, transmute};

use windows::core::{s, w};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    CreateRemoteThread, GetExitCodeThread, OpenProcess, WaitForSingleObject, INFINITE,
    LPTHREAD_START_ROUTINE, PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION,
    PROCESS_VM_READ, PROCESS_VM_WRITE,
};

const TARGET: &str = "Kindle.exe";

fn log(msg: &str) {
    use std::io::Write;
    #[cfg(debug_assertions)]
    eprintln!("[kokoro-inject] {msg}");
    if let Ok(dir) = std::env::var("TEMP") {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{dir}\\kokoro-inject.log"))
        {
            let _ = writeln!(f, "[kokoro-inject] {msg}");
        }
    }
}

fn find_pid(name: &str) -> Option<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut pe = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = None;
        if Process32FirstW(snap, &mut pe).is_ok() {
            loop {
                let end = pe.szExeFile.iter().position(|&c| c == 0).unwrap_or(pe.szExeFile.len());
                let exe = String::from_utf16_lossy(&pe.szExeFile[..end]);
                if exe.eq_ignore_ascii_case(name) {
                    found = Some(pe.th32ProcessID);
                    break;
                }
                if Process32NextW(snap, &mut pe).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Inject `dll_abs` into the target. Returns Err with a reason; never panics.
unsafe fn inject(pid: u32, dll_abs: &str) -> Result<(), String> {
    let hproc = OpenProcess(
        PROCESS_CREATE_THREAD
            | PROCESS_QUERY_INFORMATION
            | PROCESS_VM_OPERATION
            | PROCESS_VM_WRITE
            | PROCESS_VM_READ,
        false,
        pid,
    )
    .map_err(|e| format!("OpenProcess({pid}) failed: {e:?} (Kindle running elevated?)"))?;

    let result = (|| -> Result<u32, String> {
        let mut wpath: Vec<u16> = dll_abs.encode_utf16().chain(std::iter::once(0)).collect();
        let nbytes = wpath.len() * 2;

        let remote = VirtualAllocEx(hproc, None, nbytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        if remote.is_null() {
            return Err("VirtualAllocEx failed".into());
        }
        let write = WriteProcessMemory(hproc, remote, wpath.as_mut_ptr() as *const c_void, nbytes, None)
            .map_err(|e| format!("WriteProcessMemory failed: {e:?}"));
        if let Err(e) = write {
            let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
            return Err(e);
        }

        // LoadLibraryW is at the same address in every WOW64 process this boot.
        let k32 = GetModuleHandleW(w!("kernel32.dll")).map_err(|e| format!("kernel32: {e:?}"))?;
        let load = GetProcAddress(k32, s!("LoadLibraryW"));
        let start: LPTHREAD_START_ROUTINE = transmute(load);

        let thread = CreateRemoteThread(hproc, None, 0, start, Some(remote as *const c_void), 0, None)
            .map_err(|e| format!("CreateRemoteThread failed: {e:?}"));
        let hthread = match thread {
            Ok(h) => h,
            Err(e) => {
                let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
                return Err(e);
            }
        };

        WaitForSingleObject(hthread, INFINITE);
        let mut exit: u32 = 0;
        let _ = GetExitCodeThread(hthread, &mut exit);
        let _ = VirtualFreeEx(hproc, remote, 0, MEM_RELEASE);
        let _ = CloseHandle(hthread);
        // exit = low 32 bits of the HMODULE from LoadLibraryW; 0 means the load failed.
        Ok(exit)
    })();

    let _ = CloseHandle(hproc);
    match result {
        Ok(0) => Err("LoadLibraryW returned NULL in target (bad path / arch?)".into()),
        Ok(h) => {
            log(&format!("injected OK into pid {pid} (module low32 = 0x{h:x})"));
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn main() {
    let dll = std::env::args().nth(1).unwrap_or_else(|| {
        // Dev fallback; the host always passes an explicit staged path.
        r"..\kokoro-hook\target\i686-pc-windows-msvc\release\kokoro_hook.dll".into()
    });
    let dll_abs = std::fs::canonicalize(&dll)
        .map(|p| p.to_string_lossy().trim_start_matches(r"\\?\").to_string())
        .unwrap_or(dll.clone());

    if !std::path::Path::new(&dll_abs).exists() {
        log(&format!("hook DLL not found: {dll_abs}"));
        std::process::exit(2);
    }
    let Some(pid) = find_pid(TARGET) else {
        log(&format!("{TARGET} not running"));
        std::process::exit(1);
    };

    match unsafe { inject(pid, &dll_abs) } {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            log(&e);
            std::process::exit(3);
        }
    }
}
