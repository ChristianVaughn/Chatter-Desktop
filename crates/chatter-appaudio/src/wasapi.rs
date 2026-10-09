//! Windows backend: WASAPI process loopback.
//!
//! Since Windows 10 build 20348 (Server 2022) / Windows 11, the audio engine can produce a
//! loopback stream for *a process tree* instead of a whole endpoint: activate an
//! `IAudioClient` on the virtual device `VAD\Process_Loopback` with
//! `AUDIOCLIENT_ACTIVATION_PARAMS { PROCESS_LOOPBACK, { pid, INCLUDE | EXCLUDE tree } }`.
//! The mix happens inside the audio engine, across all endpoints the tree plays to, so we
//! never see per-session streams. Notes from practice and the SDK sample
//! (`ApplicationLoopback`):
//!
//! - Activation is asynchronous (`ActivateAudioInterfaceAsync`) and the completion handler is
//!   called on an MTA worker thread, so the handler must be agile (windows-rs `implement`
//!   objects are) and we just wait for its signal.
//! - The virtual device has no mix format (`GetMixFormat` is `E_NOTIMPL`); the client states
//!   the format it wants, and `AUTOCONVERTPCM` makes the engine resample/convert to it. We
//!   ask for 48 kHz stereo float (falling back to 16-bit PCM), which is our output format.
//! - When the target is silent or not playing, the engine typically delivers *no packets*;
//!   [`Pacer`] keeps the output clock running with zeros.
//!
//! Everything runs on one dedicated MTA thread per capture; `list_apps` uses a short-lived
//! MTA thread so the caller's COM apartment state is never touched.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _};
use windows::core::{implement, Interface, HRESULT, PCWSTR, PWSTR};
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, S_OK};
use windows::Win32::Media::Audio::{
    eRender, ActivateAudioInterfaceAsync, AudioSessionStateExpired,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
    IAudioSessionControl, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    DEVICE_STATE_ACTIVE, PROCESS_LOOPBACK_MODE, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, WAVE_FORMAT_PCM,
};
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, BLOB, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, OpenProcess,
    QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

use crate::chunk::{append_stereo, Chunker, Pacer, SampleFormat, SAMPLE_RATE};
use crate::duck::{Filter, Session, VolumeBackend};
use crate::proc_tree::{
    exclusion_root, parse_window_source_id, title_from_exe, ProcEntry, ProcTable,
};
use crate::{AudioApp, Capabilities, Capture, OnAudio, Target};

/// First build with process loopback (Windows Server 2022; every Windows 11 build is newer).
const MIN_BUILD: u32 = 20348;
/// Requested engine buffer (100 ns units): 100 ms of slack against scheduling hiccups.
/// Latency is set by the event period, not by this.
const BUFFER_HNS: i64 = 1_000_000;
/// How long to wait for the asynchronous activation.
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(5);
/// Event wait granularity; also bounds silence-fill latency and stop latency.
const WAIT_MS: u32 = 5;
/// After a stream error (e.g. audio service restart), retry this often while emitting silence.
const RETRY_EVERY: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------------------------
// Capabilities / discovery

fn os_build() -> u32 {
    static BUILD: OnceLock<u32> = OnceLock::new();
    *BUILD.get_or_init(|| {
        // RtlGetVersion, unlike GetVersionEx, is not subject to manifest-based version lies.
        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is a correctly sized, writable OSVERSIONINFOW.
        let status = unsafe { RtlGetVersion(&mut info) };
        if status.is_ok() {
            info.dwBuildNumber
        } else {
            0
        }
    })
}

pub(crate) fn capabilities() -> Capabilities {
    let ok = os_build() >= MIN_BUILD;
    Capabilities {
        per_app: ok,
        all_except: ok,
    }
}

pub(crate) fn window_pid(source_id: &str) -> Option<u32> {
    let id = parse_window_source_id(source_id)?;
    let hwnd = HWND(id as usize as *mut c_void);
    let mut pid = 0u32;
    // SAFETY: GetWindowThreadProcessId validates the handle and returns 0 for invalid ones.
    let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (tid != 0 && pid != 0).then_some(pid)
}

/// Closes a Win32 handle on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: we own the handle and close it exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn utf16_until_nul(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// All processes (pid, parent pid, exe name) via a ToolHelp snapshot.
fn snapshot() -> ProcTable {
    let mut entries = Vec::new();
    // SAFETY: standard ToolHelp iteration over a snapshot handle we own.
    unsafe {
        if let Ok(h) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            let h = OwnedHandle(h);
            let mut pe = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut ok = Process32FirstW(h.0, &mut pe).is_ok();
            while ok {
                entries.push(ProcEntry {
                    pid: pe.th32ProcessID,
                    ppid: pe.th32ParentProcessID,
                    exe: utf16_until_nul(&pe.szExeFile),
                });
                ok = Process32NextW(h.0, &mut pe).is_ok();
            }
        }
    }
    ProcTable::new(entries)
}

fn process_image_path(pid: u32) -> Option<String> {
    // SAFETY: plain Win32 calls with a correctly sized buffer; the handle is closed by
    // OwnedHandle.
    unsafe {
        let h = OwnedHandle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
            .ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// The `FileDescription` from an executable's version resource ("Google Chrome",
/// "Spotify", ...), in the file's first listed language (else US English / Unicode).
fn file_description(path: &str) -> Option<String> {
    let wpath: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the version APIs read into `data`, which outlives every pointer VerQueryValueW
    // hands back into it; lengths are checked before slices are formed.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wpath.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wpath.as_ptr()), None, size, data.as_mut_ptr().cast()).ok()?;

        let query = |sub: &str| -> Option<(*const c_void, u32)> {
            let wsub: Vec<u16> = sub.encode_utf16().chain(Some(0)).collect();
            let mut ptr: *mut c_void = std::ptr::null_mut();
            let mut len = 0u32;
            let ok = VerQueryValueW(
                data.as_ptr().cast(),
                PCWSTR(wsub.as_ptr()),
                &mut ptr,
                &mut len,
            );
            (ok.as_bool() && !ptr.is_null() && len > 0).then_some((ptr as *const c_void, len))
        };

        let mut langs = Vec::new();
        if let Some((ptr, len)) = query("\\VarFileInfo\\Translation") {
            let pairs = std::slice::from_raw_parts(ptr as *const u16, (len / 2) as usize);
            for pair in pairs.chunks_exact(2) {
                langs.push(format!("{:04x}{:04x}", pair[0], pair[1]));
            }
        }
        langs.push("040904b0".into());
        langs.push("040904e4".into());
        for lang in langs {
            if let Some((ptr, len)) = query(&format!("\\StringFileInfo\\{lang}\\FileDescription")) {
                // `len` is in characters and includes the terminator.
                let chars = std::slice::from_raw_parts(ptr as *const u16, len as usize);
                let s = utf16_until_nul(chars).trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
        None
    }
}

/// Picks a display name for an app: session display name, version-resource description,
/// file stem.
fn friendly_name(pid: u32, session_name: Option<&str>, table: &ProcTable) -> String {
    if let Some(n) = session_name {
        // "@%SystemRoot%\...,-202"-style values are unresolved resource references.
        if !n.is_empty() && !n.starts_with('@') {
            return n.to_string();
        }
    }
    let path = process_image_path(pid);
    if let Some(desc) = path.as_deref().and_then(file_description) {
        return desc;
    }
    let exe = path.or_else(|| table.get(pid).map(|e| e.exe.clone()));
    match exe {
        Some(exe) if !exe.is_empty() => title_from_exe(&exe),
        _ => format!("Process {pid}"),
    }
}

/// Runs `f` on a fresh MTA thread (so the caller's COM state is irrelevant) and returns its
/// result.
fn on_mta_thread<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    std::thread::Builder::new()
        .name("chatter-appaudio-com".into())
        .spawn(move || {
            // SAFETY: initialises COM for this fresh thread; balanced below.
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
            let r = f();
            // SAFETY: balances the successful CoInitializeEx; all COM objects created by `f`
            // have been dropped when it returned.
            unsafe { CoUninitialize() };
            r
        })
        .context("spawning COM thread")?
        .join()
        .map_err(|_| anyhow!("COM thread panicked"))?
}

/// One audio session on some active render endpoint.
struct SessionRef {
    ctl: IAudioSessionControl,
    ctl2: IAudioSessionControl2,
}

/// Every non-expired audio session on every active render endpoint. Must run on a COM
/// thread.
fn render_sessions() -> anyhow::Result<Vec<SessionRef>> {
    let mut out = Vec::new();
    // SAFETY: COM calls on interfaces we own.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .context("MMDeviceEnumerator")?;
        let devices = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .context("EnumAudioEndpoints")?;
        for i in 0..devices.GetCount()? {
            let sessions = devices
                .Item(i)
                .and_then(|dev| dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None))
                .and_then(|mgr| mgr.GetSessionEnumerator());
            let sessions = match sessions {
                Ok(s) => s,
                Err(e) => {
                    log::debug!("appaudio: endpoint {i}: no session enumerator: {e}");
                    continue;
                }
            };
            for j in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(ctl) = sessions.GetSession(j) else {
                    continue;
                };
                if ctl.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
                    continue;
                }
                let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                out.push(SessionRef { ctl, ctl2 });
            }
        }
    }
    Ok(out)
}

/// Converts and frees a COM-allocated string.
///
/// # Safety
/// `p` must be null or a NUL-terminated string allocated with `CoTaskMemAlloc`.
unsafe fn take_co_string(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: per the function contract.
    unsafe {
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

pub(crate) fn list_apps(exclude: &[u32]) -> Vec<AudioApp> {
    let exclude = exclude.to_vec();
    match on_mta_thread(move || list_sessions(&exclude)) {
        Ok(apps) => apps,
        Err(e) => {
            log::warn!("appaudio: listing audio sessions failed: {e:#}");
            Vec::new()
        }
    }
}

/// Enumerates audio sessions on every active render endpoint.
fn list_sessions(exclude: &[u32]) -> anyhow::Result<Vec<AudioApp>> {
    let table = snapshot();
    let excluded = table.trees(exclude);
    // (app root pid, session display name)
    let mut found: Vec<(u32, Option<String>)> = Vec::new();
    for s in render_sessions()? {
        // SAFETY: COM calls on interfaces we own, on an MTA thread.
        unsafe {
            if s.ctl2.IsSystemSoundsSession() == S_OK {
                continue;
            }
            let pid = s.ctl2.GetProcessId().unwrap_or(0);
            if pid == 0 || excluded.contains(&pid) {
                continue;
            }
            let display = s.ctl.GetDisplayName().ok().and_then(|p| take_co_string(p));
            found.push((table.app_root(pid), display));
        }
    }

    // Prefer a non-empty display name when an app has several sessions.
    found.sort_by_key(|(pid, name)| (*pid, name.as_deref().is_none_or(str::is_empty)));
    found.dedup_by_key(|(pid, _)| *pid);
    Ok(found
        .into_iter()
        .map(|(pid, display)| AudioApp {
            pid,
            name: friendly_name(pid, display.as_deref(), &table),
        })
        .collect())
}

// ---------------------------------------------------------------------------------------------
// Capture

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    done: mpsc::SyncSender<()>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        _op: windows::core::Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let _ = self.done.try_send(());
        Ok(())
    }
}

/// Activates a process-loopback `IAudioClient` for `pid` in `mode`.
fn activate(pid: u32, mode: PROCESS_LOOPBACK_MODE) -> anyhow::Result<IAudioClient> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: mode,
            },
        },
    };
    // windows-rs gives PROPVARIANT a Drop that calls PropVariantClear, which would hand our
    // stack-allocated blob to CoTaskMemFree (heap corruption). It owns nothing, so never drop.
    let mut pv = std::mem::ManuallyDrop::new(PROPVARIANT::default());
    // SAFETY: a VT_BLOB PROPVARIANT pointing at `params`, which outlives the activation (we
    // wait for completion below). The PROPVARIANT is never cleared, so the blob is not freed.
    unsafe {
        let inner = &mut *pv.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob = BLOB {
            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: &params as *const _ as *mut u8,
        };
    }

    let (tx, rx) = mpsc::sync_channel(1);
    let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler { done: tx }.into();
    // SAFETY: all pointers are valid for the duration of the call / activation.
    let op = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*pv),
            &handler,
        )
    }
    .context("ActivateAudioInterfaceAsync")?;
    rx.recv_timeout(ACTIVATE_TIMEOUT)
        .map_err(|_| anyhow!("process loopback activation timed out"))?;

    let mut hr = HRESULT(0);
    let mut unk = None;
    // SAFETY: out-pointers are valid locals.
    unsafe { op.GetActivateResult(&mut hr, &mut unk) }.context("GetActivateResult")?;
    hr.ok().context("process loopback activation failed")?;
    let unk = unk.ok_or_else(|| anyhow!("activation returned no interface"))?;
    Ok(unk.cast::<IAudioClient>()?)
}

fn wave_format(format: SampleFormat) -> WAVEFORMATEX {
    let (tag, bits) = match format {
        SampleFormat::F32 => (WAVE_FORMAT_IEEE_FLOAT, 32u16),
        SampleFormat::I16 => (WAVE_FORMAT_PCM, 16),
    };
    let block_align = 2 * bits / 8;
    WAVEFORMATEX {
        wFormatTag: tag as u16,
        nChannels: 2,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: bits,
        cbSize: 0,
    }
}

/// An initialised, started process-loopback stream.
struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: OwnedHandle,
    format: SampleFormat,
}

impl Stream {
    fn open(pid: u32, mode: PROCESS_LOOPBACK_MODE) -> anyhow::Result<Stream> {
        let mut last_err = None;
        // A fresh activation per attempt: a client whose Initialize failed is not reused.
        for format in [SampleFormat::F32, SampleFormat::I16] {
            let client = activate(pid, mode)?;
            let wfx = wave_format(format);
            let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            // SAFETY: `wfx` is a valid WAVEFORMATEX for the duration of the call.
            let init = unsafe {
                client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_HNS, 0, &wfx, None)
            };
            if let Err(e) = init {
                log::debug!("appaudio: Initialize({format:?}) failed: {e}");
                last_err = Some(e);
                continue;
            }
            // SAFETY: plain COM / Win32 calls on objects we own.
            unsafe {
                let event = OwnedHandle(CreateEventW(None, false, false, PCWSTR::null())?);
                client.SetEventHandle(event.0).context("SetEventHandle")?;
                let capture: IAudioCaptureClient = client.GetService().context("GetService")?;
                client.Start().context("IAudioClient::Start")?;
                return Ok(Stream {
                    client,
                    capture,
                    event,
                    format,
                });
            }
        }
        Err(anyhow!(last_err.expect("at least one format tried")))
            .context("IAudioClient::Initialize")
    }

    /// Drains every available packet into `out` (stereo f32), returning the frame count of
    /// each packet via `on_packet`.
    fn drain(
        &self,
        conv: &mut Vec<f32>,
        on_packet: &mut dyn FnMut(&[f32], u64),
    ) -> anyhow::Result<()> {
        let block = self.format.bytes() * 2;
        // SAFETY: GetBuffer/ReleaseBuffer pairs on our capture client; the buffer is read only
        // between them, with the size the engine reported.
        unsafe {
            loop {
                let n = self.capture.GetNextPacketSize()?;
                if n == 0 {
                    return Ok(());
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                conv.clear();
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    conv.resize(frames as usize * 2, 0.0);
                } else {
                    let bytes = std::slice::from_raw_parts(data, frames as usize * block);
                    append_stereo(bytes, self.format, 2, conv);
                }
                self.capture.ReleaseBuffer(frames)?;
                on_packet(conv, u64::from(frames));
            }
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: stopping our own client; errors are irrelevant at teardown.
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

pub(crate) struct WasapiCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WasapiCapture {
    pub(crate) fn start(target: Target, on_audio: OnAudio) -> anyhow::Result<Self> {
        if !capabilities().per_app {
            bail!(
                "application audio capture needs Windows 10 build {MIN_BUILD} or Windows 11 \
                 (this is build {})",
                os_build()
            );
        }
        let (pid, mode) = match target {
            Target::App { pid } => (pid, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE),
            Target::AllExcept { pids } => {
                let root = if pids.is_empty() {
                    std::process::id()
                } else {
                    exclusion_root(&pids, &snapshot()).unwrap_or(pids[0])
                };
                if pids.len() > 1 {
                    log::debug!("appaudio: excluding process tree of {root} (of {pids:?})");
                }
                (root, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE)
            }
        };

        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("chatter-appaudio".into())
                .spawn(move || capture_thread(pid, mode, on_audio, stop, ready_tx))
                .context("spawning capture thread")?
        };
        let ready = ready_rx
            .recv_timeout(ACTIVATE_TIMEOUT * 3)
            .unwrap_or_else(|_| Err(anyhow!("capture thread did not start")));
        let mut cap = WasapiCapture {
            stop,
            thread: Some(thread),
        };
        match ready {
            Ok(()) => Ok(cap),
            Err(e) => {
                cap.stop();
                Err(e)
            }
        }
    }
}

impl Capture for WasapiCapture {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn capture_thread(
    pid: u32,
    mode: PROCESS_LOOPBACK_MODE,
    mut on_audio: OnAudio,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<anyhow::Result<()>>,
) {
    // SAFETY: COM init for this thread, balanced at the end.
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
        let _ = ready.send(Err(anyhow!(e).context("CoInitializeEx")));
        return;
    }
    // Pro-audio scheduling (MMCSS) so a busy machine does not starve the capture.
    let mut task_index = 0u32;
    // SAFETY: plain Win32 call; reverted below.
    let mmcss =
        unsafe { AvSetMmThreadCharacteristicsW(windows::core::w!("Audio"), &mut task_index) }.ok();

    match Stream::open(pid, mode) {
        Err(e) => {
            let _ = ready.send(Err(e));
        }
        Ok(stream) => {
            log::info!(
                "appaudio: capturing process tree {pid} ({}) as {:?}",
                if mode == PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE {
                    "include"
                } else {
                    "exclude"
                },
                stream.format
            );
            let _ = ready.send(Ok(()));
            run(stream, pid, mode, &mut *on_audio, &stop);
        }
    }

    if let Some(h) = mmcss {
        // SAFETY: reverting the MMCSS registration made above.
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(h);
        }
    }
    // SAFETY: balances CoInitializeEx; every COM object of this thread is dropped by now.
    unsafe { CoUninitialize() };
}

fn run(
    stream: Stream,
    pid: u32,
    mode: PROCESS_LOOPBACK_MODE,
    on_audio: &mut dyn FnMut(&[f32]),
    stop: &AtomicBool,
) {
    let mut stream = Some(stream);
    let mut chunker = Chunker::new();
    let mut pacer = Pacer::new(Instant::now(), 0);
    let mut conv: Vec<f32> = Vec::with_capacity(4096);
    let mut last_retry = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        match &stream {
            Some(s) => {
                // Signalled or timed out, either way drain whatever is there: the timeout is
                // what drives silence-fill while the engine delivers nothing.
                // SAFETY: waiting on our own event handle.
                unsafe { WaitForSingleObject(s.event.0, WAIT_MS) };
                let result = s.drain(&mut conv, &mut |samples, frames| {
                    chunker.push(samples, on_audio);
                    pacer.on_data(Instant::now(), frames, chunker.position());
                });
                if let Err(e) = result {
                    log::warn!("appaudio: capture stream failed ({e:#}); retrying");
                    stream = None;
                    last_retry = Instant::now();
                }
            }
            None => {
                std::thread::sleep(Duration::from_millis(u64::from(WAIT_MS)));
                if last_retry.elapsed() >= RETRY_EVERY {
                    last_retry = Instant::now();
                    match Stream::open(pid, mode) {
                        Ok(s) => {
                            log::info!("appaudio: capture stream reopened");
                            stream = Some(s);
                        }
                        Err(e) => log::debug!("appaudio: reopen failed: {e:#}"),
                    }
                }
            }
        }
        let due = pacer.silence_due(Instant::now(), chunker.position());
        if due > 0 {
            chunker.push_silence(due, on_audio);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Ducking

/// Event context passed to `SetMasterVolume`, so volume-change notifications caused by
/// Chatter are recognisable (by other tools, or by a future event sink of ours).
const DUCK_EVENT_CONTEXT: windows::core::GUID =
    windows::core::GUID::from_u128(0x6c1f_3a52_9d0e_4b7a_a3c4_43c8_a7d1_9e25);

/// Session volumes via `ISimpleAudioVolume` (works on any Windows 7+; sessions are keyed by
/// their instance identifier, which is unique per session and endpoint, and belong to the app
/// their session identifier names).
///
/// The system-sounds session (no owning process) is ducked too: notification dings are just
/// as distracting during a call.
struct WinVolumes {
    filter: Filter,
    controls: HashMap<String, ISimpleAudioVolume>,
}

impl VolumeBackend for WinVolumes {
    type Key = String;
    type Vol = f32;

    fn vol_to_vec(v: &f32) -> Vec<f64> {
        vec![f64::from(*v)]
    }

    fn vol_from_vec(v: &[f64]) -> Option<f32> {
        v.first().map(|x| (*x as f32).clamp(0.0, 1.0))
    }

    fn scan(&mut self) -> anyhow::Result<Vec<Session<String, f32>>> {
        let table = snapshot();
        self.controls.clear();
        let mut out = Vec::new();
        for s in render_sessions()? {
            // SAFETY: COM calls on interfaces we own, on this MTA thread.
            unsafe {
                let pid = s.ctl2.GetProcessId().ok();
                if !self.filter.allows(pid, &table) {
                    continue;
                }
                let Some(id) = s
                    .ctl2
                    .GetSessionInstanceIdentifier()
                    .ok()
                    .and_then(|p| take_co_string(p))
                else {
                    continue;
                };
                // Endpoint, executable and grouping, without the process: what Windows keeps
                // the app's volume under from one run to the next.
                let app = s
                    .ctl2
                    .GetSessionIdentifier()
                    .ok()
                    .and_then(|p| take_co_string(p))
                    .unwrap_or_default();
                let Ok(vol) = s.ctl.cast::<ISimpleAudioVolume>() else {
                    continue;
                };
                let Ok(level) = vol.GetMasterVolume() else {
                    continue;
                };
                self.controls.insert(id.clone(), vol);
                out.push(Session {
                    key: id,
                    app,
                    vol: level,
                });
            }
        }
        Ok(out)
    }

    fn scaled(original: &f32, factor: f32) -> f32 {
        if factor >= 1.0 {
            *original
        } else {
            (original * factor.max(0.0)).clamp(0.0, 1.0)
        }
    }

    fn same(a: &f32, b: &f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn set(&mut self, key: &String, vol: &f32) -> bool {
        match self.controls.get(key) {
            // SAFETY: COM call on an interface we own.
            Some(c) => unsafe { c.SetMasterVolume(*vol, &DUCK_EVENT_CONTEXT) }.is_ok(),
            None => false,
        }
    }
}

/// Body of the ducking thread.
pub(crate) fn run_ducker(
    filter: Filter,
    journal: Option<std::path::PathBuf>,
    rx: mpsc::Receiver<crate::duck::Cmd>,
) {
    // SAFETY: COM init for this thread, balanced below after the backend is dropped.
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
        log::error!("appaudio: ducking unavailable: CoInitializeEx: {e}");
        // Keep draining so `set` stays harmless.
        while rx.recv().is_ok() {}
        return;
    }
    crate::duck::run(
        WinVolumes {
            filter,
            controls: HashMap::new(),
        },
        journal,
        rx,
    );
    // SAFETY: balances CoInitializeEx; the backend (and its COM objects) is dropped.
    unsafe { CoUninitialize() };
}

#[cfg(test)]
mod tests {
    //! Hardware tests: they need a Windows 11 machine with an active audio output device.
    //! Run with `cargo test -p chatter-appaudio -- --ignored --nocapture --test-threads=1`.

    use super::*;
    use crate::{AppAudioCapture, CHUNK_SAMPLES};
    use std::sync::Mutex;

    #[test]
    fn build_number_is_known() {
        assert!(os_build() > 0);
    }

    #[test]
    fn wave_formats_are_consistent() {
        let f = wave_format(SampleFormat::F32);
        assert_eq!(
            (f.nBlockAlign, f.nAvgBytesPerSec, f.wBitsPerSample),
            (8, 384_000, 32)
        );
        let i = wave_format(SampleFormat::I16);
        assert_eq!(
            (i.nBlockAlign, i.nAvgBytesPerSec, i.wFormatTag),
            (4, 192_000, 1)
        );
    }

    #[test]
    fn window_pid_resolves_shell_window() {
        // SAFETY: plain Win32 query.
        let hwnd = unsafe { windows::Win32::UI::WindowsAndMessaging::GetShellWindow() };
        if hwnd.is_invalid() {
            return; // no interactive desktop (service / CI session)
        }
        let pid = window_pid(&format!("window:{}:0", hwnd.0 as usize));
        let explorer = pid.and_then(|p| snapshot().get(p).map(|e| e.exe.to_lowercase()));
        assert_eq!(explorer.as_deref(), Some("explorer.exe"));
    }

    #[test]
    fn window_pid_rejects_garbage() {
        assert_eq!(window_pid("screen:0:0"), None);
        assert_eq!(window_pid("window:1:0"), None);
    }

    #[test]
    #[ignore = "needs audio hardware"]
    fn list_apps_runs() {
        let apps = crate::list_apps(&[std::process::id()]);
        println!("{} apps with audio sessions:", apps.len());
        for a in &apps {
            println!("  {:>6}  {}", a.pid, a.name);
        }
        assert!(apps
            .iter()
            .all(|a| a.pid != 0 && a.pid != std::process::id()));
    }

    type Collected = Arc<Mutex<(usize, Vec<f32>)>>;

    /// Starts a capture that records every chunk (count, samples).
    fn recorder(target: Target) -> (AppAudioCapture, Collected) {
        let got: Collected = Arc::default();
        let sink = got.clone();
        let cap = AppAudioCapture::start(target, move |s: &[f32]| {
            assert_eq!(s.len(), CHUNK_SAMPLES);
            let mut g = sink.lock().unwrap();
            g.0 += 1;
            g.1.extend_from_slice(s);
        })
        .expect("start capture");
        (cap, got)
    }

    fn take(got: &Collected) -> (usize, Vec<f32>) {
        let g = got.lock().unwrap();
        (g.0, g.1.clone())
    }

    /// Asserts ~100 chunks/s over one second for `target`.
    fn assert_paced(target: Target) {
        let t = Instant::now();
        let (cap, got) = recorder(target);
        std::thread::sleep(Duration::from_secs(1));
        drop(cap);
        let secs = t.elapsed().as_secs_f64();
        let (chunks, samples) = take(&got);
        let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        println!(
            "{chunks} chunks in {secs:.3}s ({:.1}/s), peak {peak:.4}",
            chunks as f64 / secs
        );
        // Startup (activation) eats a little of the second; allow some slack either way.
        assert!((85..=115).contains(&chunks), "got {chunks} chunks");
    }

    #[test]
    #[ignore = "needs Windows 11 audio stack"]
    fn all_except_self_paces() {
        assert_paced(Target::AllExcept {
            pids: vec![std::process::id()],
        });
    }

    #[test]
    #[ignore = "needs Windows 11 audio stack"]
    fn silent_app_still_paces() {
        // This test process plays nothing, so the OS delivers no packets: all silence-fill.
        assert_paced(Target::App {
            pid: std::process::id(),
        });
    }

    /// Writes a 48 kHz mono 16-bit WAV with a `freq` Hz sine.
    fn write_tone(path: &std::path::Path, freq: f32, secs: f32) {
        let n = (48_000.0 * secs) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + n * 2).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&1u16.to_le_bytes()); // mono
        b.extend_from_slice(&48_000u32.to_le_bytes());
        b.extend_from_slice(&96_000u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(n * 2).to_le_bytes());
        for i in 0..n {
            let s = (2.0 * std::f32::consts::PI * freq * i as f32 / 48_000.0).sin() * 0.5;
            b.extend_from_slice(&((s * 32_767.0) as i16).to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    /// Spawns PowerShell playing a 3 s 1 kHz tone through the normal audio-session path.
    fn spawn_tone_player(dir: &std::path::Path, secs: f32) -> std::process::Child {
        std::fs::create_dir_all(dir).unwrap();
        let wav = dir.join("tone.wav");
        write_tone(&wav, 1000.0, secs);
        std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "(New-Object System.Media.SoundPlayer '{}').PlaySync()",
                    wav.display()
                ),
            ])
            .spawn()
            .expect("spawn powershell")
    }

    /// Power of `freq` in `x` (Goertzel), normalised per sample.
    fn tone_power(x: &[f32], freq: f32) -> f32 {
        let w = 2.0 * std::f32::consts::PI * freq / 48_000.0;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0f32, 0f32);
        for &v in x {
            let s0 = v + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coeff * s1 * s2) / (x.len().max(1) as f32).powi(2)
    }

    /// (seconds of non-silent audio, its RMS, zero-crossing frequency) of the left channel.
    fn analyse(samples: &[f32]) -> (f32, f32, f32) {
        let left: Vec<f32> = samples.iter().step_by(2).copied().collect();
        let loud: Vec<f32> = left
            .chunks(480)
            .filter(|c| c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32 > 1e-4)
            .flatten()
            .copied()
            .collect();
        let rms = (loud.iter().map(|s| s * s).sum::<f32>() / loud.len().max(1) as f32).sqrt();
        let crossings = loud
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        let secs = loud.len() as f32 / 48_000.0;
        (secs, rms, crossings as f32 / 2.0 / secs.max(1e-6))
    }

    #[test]
    #[ignore = "needs Windows 11 audio stack and an output device"]
    fn captures_child_process_tone_and_excludes_it() {
        let dir = std::env::temp_dir().join(format!("chatter-appaudio-{}", std::process::id()));
        let mut child = spawn_tone_player(&dir, 3.0);
        let pid = child.id();
        let (app, app_got) = recorder(Target::App { pid });
        let (rest, rest_got) = recorder(Target::AllExcept { pids: vec![pid] });
        std::thread::sleep(Duration::from_secs(5));
        drop(app);
        drop(rest);
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);

        let (chunks, samples) = take(&app_got);
        let (secs, rms, freq) = analyse(&samples);
        let p_app = tone_power(
            &samples.iter().step_by(2).copied().collect::<Vec<_>>(),
            1000.0,
        );
        println!("App{{{pid}}}: {chunks} chunks; {secs:.2}s non-silent, rms {rms:.3}, ~{freq:.1} Hz, 1 kHz power {p_app:.2e}");
        assert!(secs > 1.5, "too little audio captured ({secs:.2}s)");
        assert!((freq - 1000.0).abs() < 20.0, "frequency {freq}");

        let (chunks, samples) = take(&rest_got);
        let p_rest = tone_power(
            &samples.iter().step_by(2).copied().collect::<Vec<_>>(),
            1000.0,
        );
        println!("AllExcept{{{pid}}}: {chunks} chunks; 1 kHz power {p_rest:.2e}");
        assert!(chunks > 400, "exclude capture stalled ({chunks} chunks)");
        assert!(
            p_rest < p_app / 100.0,
            "excluded tone leaked: {p_rest:e} vs {p_app:e}"
        );
    }

    /// Master volume of the first session owned by `pid`.
    fn session_volume(pid: u32) -> Option<f32> {
        on_mta_thread(move || {
            for s in render_sessions()? {
                // SAFETY: COM calls on this MTA thread.
                unsafe {
                    if s.ctl2.GetProcessId().ok() == Some(pid) {
                        let v: ISimpleAudioVolume = s.ctl.cast()?;
                        return Ok(Some(v.GetMasterVolume()?));
                    }
                }
            }
            Ok(None)
        })
        .ok()
        .flatten()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    #[ignore = "needs an audio output device"]
    fn ducks_and_restores_child_session() {
        use crate::duck::{Ducker, Filter};
        let dir = std::env::temp_dir().join(format!("chatter-duck-{}", std::process::id()));
        let mut child = spawn_tone_player(&dir, 8.0);
        let pid = child.id();
        let (cap, got) = recorder(Target::App { pid });
        let len = || got.lock().unwrap().1.len();
        let sleep = |ms| std::thread::sleep(Duration::from_millis(ms));

        let t = Instant::now();
        let orig = loop {
            if let Some(v) = session_volume(pid) {
                break v;
            }
            assert!(
                t.elapsed() < Duration::from_secs(8),
                "child session never appeared"
            );
            sleep(50);
        };
        sleep(500);

        // Exclusion wins: the child is in this test process's tree.
        {
            let d = Ducker::with_filter(
                Filter {
                    exclude: vec![std::process::id()],
                    only: Some(vec![pid]),
                },
                None,
            );
            d.set(Some(0.2));
            sleep(400);
            let v = session_volume(pid).unwrap();
            println!("excluded tree: volume {v:.3} (original {orig:.3})");
            assert!((v - orig).abs() < 1e-3);
        }

        let a0 = len();
        sleep(500);
        let a1 = len();

        let d = Ducker::with_filter(
            Filter {
                exclude: vec![],
                only: Some(vec![pid]),
            },
            None,
        );
        d.set(Some(0.2));
        d.set(Some(0.2)); // repeated value: no-op
        sleep(300);
        let ducked = session_volume(pid).unwrap();
        let b0 = len();
        sleep(500);
        let b1 = len();

        d.set(None);
        sleep(150);
        let mid = session_volume(pid).unwrap();
        sleep(350);
        let restored = session_volume(pid).unwrap();
        let c0 = len();
        sleep(400);
        let c1 = len();

        // Drop restores synchronously, without waiting for a ramp.
        d.set(Some(0.3));
        sleep(300);
        let ducked2 = session_volume(pid).unwrap();
        drop(d);
        let after_drop = session_volume(pid).unwrap();

        drop(cap);
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);

        let samples = got.lock().unwrap().1.clone();
        let (ra, rb, rc) = (
            rms(&samples[a0..a1]),
            rms(&samples[b0..b1]),
            rms(&samples[c0..c1]),
        );
        println!(
            "volume: original {orig:.3}, ducked {ducked:.3}, mid-restore {mid:.3}, restored {restored:.3}; \
             second duck {ducked2:.3}, after drop {after_drop:.3}"
        );
        println!(
            "capture rms: before {ra:.4}, ducked {rb:.4} (ratio {:.2}), restored {rc:.4}",
            ra / rb.max(1e-9)
        );
        assert!((ducked - 0.2 * orig).abs() < 0.01, "ducked volume {ducked}");
        assert!(mid > ducked && mid < orig, "restore is ramped ({mid})");
        assert!((restored - orig).abs() < 1e-3, "restored volume {restored}");
        assert!((ducked2 - 0.3 * orig).abs() < 0.01, "second duck {ducked2}");
        assert!(
            (after_drop - orig).abs() < 1e-3,
            "drop restore {after_drop}"
        );
        let ratio = ra / rb.max(1e-9);
        assert!((4.0..6.5).contains(&ratio), "rms ratio {ratio}");
        assert!((rc / ra - 1.0).abs() < 0.1, "restored rms {rc} vs {ra}");
    }

    #[test]
    #[ignore = "needs an audio output device"]
    fn a_ducker_that_never_restored_is_undone_by_the_next() {
        use crate::duck::{Ducker, Filter};
        let dir = std::env::temp_dir().join(format!("chatter-duck-crash-{}", std::process::id()));
        let journal = dir.join("ducking.json");
        let mut child = spawn_tone_player(&dir, 8.0);
        let pid = child.id();
        let sleep = |ms| std::thread::sleep(Duration::from_millis(ms));
        let only = || Filter {
            exclude: vec![],
            only: Some(vec![pid]),
        };

        let t = Instant::now();
        let orig = loop {
            if let Some(v) = session_volume(pid) {
                break v;
            }
            assert!(
                t.elapsed() < Duration::from_secs(8),
                "child session never appeared"
            );
            sleep(50);
        };

        // Duck, then lose the ducker without its restore, as a killed engine would.
        let d = Ducker::with_filter(only(), Some(journal.clone()));
        d.set(Some(0.2));
        sleep(400);
        let ducked = session_volume(pid).unwrap();
        assert!(journal.exists(), "journal written while ducked");
        std::mem::forget(d);

        // The next ducker (the restarted engine) puts it back before anything else.
        let next = Ducker::with_filter(only(), Some(journal.clone()));
        sleep(300);
        let recovered = session_volume(pid).unwrap();
        drop(next);

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        println!(
            "volume: original {orig:.3}, ducked {ducked:.3}, after the next ducker {recovered:.3}"
        );
        assert!((ducked - 0.2 * orig).abs() < 0.01, "ducked volume {ducked}");
        assert!(
            (recovered - orig).abs() < 1e-3,
            "recovered volume {recovered}"
        );
        assert!(!journal.exists(), "journal removed once recovered");
    }
}
