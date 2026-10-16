//! WASAPI capture threads (Windows): the output device's loopback, a microphone, or
//! League's own sound through process loopback. Each source has a thread that polls its
//! capture client, places the packets on the shared QPC timeline (see `audio`) and queues
//! them for the pipe.
//!
//! Process loopback names `League of Legends.exe` only by process ID to the audio engine;
//! nothing opens the game process. The game may start late or restart after a crash, so
//! that source looks for it every couple of seconds and records silence meanwhile.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};

use super::audio::{SourceConfig, SourceKind, SourceReport, TimelineStats};

/// The shared start of every stream: sample 0 of each is this instant.
#[derive(Debug, Clone, Copy)]
pub struct Origin {
    pub instant: Instant,
    /// The same moment as a WASAPI QPC position (100 ns units).
    qpc_100ns: i64,
}

impl Origin {
    pub fn now() -> Self {
        let instant = Instant::now();
        Self {
            instant,
            qpc_100ns: imp::qpc_100ns(),
        }
    }

    /// Seconds from the origin to a WASAPI QPC position.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn seconds_to(self, qpc_100ns: u64) -> f64 {
        (qpc_100ns as i64 - self.qpc_100ns) as f64 / 1e7
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn elapsed_s(self) -> f64 {
        self.instant.elapsed().as_secs_f64()
    }
}

/// A running capture thread.
pub struct SourceThread {
    kind: SourceKind,
    handle: std::thread::JoinHandle<SourceReport>,
}

impl SourceThread {
    /// Waits for the thread (stop it first) and returns its report.
    pub fn join(self) -> SourceReport {
        let kind = self.kind;
        self.handle.join().unwrap_or_else(|_| SourceReport {
            kind,
            device: String::new(),
            stats: TimelineStats::default(),
            error: Some("the capture thread panicked".into()),
        })
    }
}

/// Opens the source on its own thread and starts it. Returns the stream's sample rate
/// once it runs, or why it couldn't open.
pub async fn spawn(
    config: SourceConfig,
    origin: Origin,
    chunks: mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
) -> Result<(SourceThread, u32), String> {
    let (ready_tx, ready_rx) = oneshot::channel();
    let kind = config.kind;
    let handle = std::thread::Builder::new()
        .name(format!("audio-{kind:?}"))
        .spawn(move || imp::run(config, origin, chunks, stop, ready_tx))
        .map_err(|e| e.to_string())?;
    let thread = SourceThread { kind, handle };
    match tokio::time::timeout(Duration::from_secs(5), ready_rx).await {
        Ok(Ok(Ok(rate))) => Ok((thread, rate)),
        Ok(Ok(Err(error))) => {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
            Err(error)
        }
        Ok(Err(_)) => Err("the capture thread ended".into()),
        // It keeps the shared stop flag and ends with the recording.
        Err(_) => Err("the audio device didn't open in time".into()),
    }
}

/// An audio endpoint, for `downplayed-dev audio`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub input: bool,
    pub default: bool,
}

/// Every active output and input device.
pub fn devices() -> Vec<Device> {
    imp::devices()
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use tokio::sync::{mpsc, oneshot};
    use windows::core::{implement, Interface, Ref, HRESULT, PCWSTR, PWSTR};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Media::Audio::{
        eCapture, eConsole, eRender, ActivateAudioInterfaceAsync, EDataFlow,
        IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
        IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
        IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
        AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
        AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
        DEVICE_STATE_ACTIVE, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
    };
    use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
    use windows::Win32::Media::Multimedia::{
        KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT,
    };
    use windows::Win32::System::Com::StructuredStorage::{
        PropVariantClear, PropVariantToStringAlloc, PROPVARIANT,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IAgileObject,
        IAgileObject_Impl, BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
    };
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::Win32::System::Variant::VT_BLOB;

    use super::super::audio::{
        send_chunk, to_bytes, to_stereo, SampleFormat, SourceConfig, SourceKind, SourceReport,
        Timeline, RATE,
    };
    use super::{Device, Origin};

    /// The capture client's buffer: room for scheduling hiccups.
    const BUFFER_100NS: i64 = 2_000_000; // 200 ms
    const POLL: Duration = Duration::from_millis(10);
    /// How often the game source looks for (or checks on) `League of Legends.exe`.
    const GAME_SCAN: Duration = Duration::from_secs(2);
    /// How long to wait before reopening a device that went away.
    const REOPEN: Duration = Duration::from_secs(1);

    pub fn qpc_100ns() -> i64 {
        let (mut count, mut frequency) = (0i64, 0i64);
        // SAFETY: plain out-parameters.
        unsafe {
            let _ = QueryPerformanceCounter(&mut count);
            let _ = QueryPerformanceFrequency(&mut frequency);
        }
        if frequency <= 0 {
            return 0;
        }
        (i128::from(count) * 10_000_000 / i128::from(frequency)) as i64
    }

    #[derive(Debug, Clone, Copy)]
    struct DeviceFormat {
        rate: u32,
        channels: usize,
        sample: SampleFormat,
    }

    /// An opened, started capture client.
    struct Stream {
        client: IAudioClient,
        capture: IAudioCaptureClient,
        format: DeviceFormat,
        /// Event-driven clients (process loopback) signal this.
        event: Option<HANDLE>,
    }

    impl Drop for Stream {
        fn drop(&mut self) {
            // SAFETY: stopping a client this stream owns; closing its event.
            unsafe {
                let _ = self.client.Stop();
                if let Some(event) = self.event {
                    let _ = CloseHandle(event);
                }
            }
        }
    }

    fn float_format(rate: u32) -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
            nChannels: 2,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 0,
        }
    }

    fn pcm16_format(rate: u32) -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        }
    }

    /// Reads a (possibly extensible) wave format.
    ///
    /// SAFETY: `format` points to a valid WAVEFORMATEX (or WAVEFORMATEXTENSIBLE when its
    /// tag says so).
    unsafe fn device_format(format: *const WAVEFORMATEX) -> Result<DeviceFormat, String> {
        let wave = unsafe { std::ptr::read_unaligned(format) };
        let tag = u32::from(wave.wFormatTag);
        let float = if tag == WAVE_FORMAT_EXTENSIBLE {
            let extensible =
                unsafe { std::ptr::read_unaligned(format.cast::<WAVEFORMATEXTENSIBLE>()) };
            let sub = extensible.SubFormat;
            sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        } else {
            tag == WAVE_FORMAT_IEEE_FLOAT
        };
        let sample = match (float, wave.wBitsPerSample) {
            (true, 32) => SampleFormat::F32,
            (false, 16) => SampleFormat::I16,
            (false, 32) => SampleFormat::I32,
            (_, bits) => return Err(format!("unsupported device format ({bits}-bit)")),
        };
        Ok(DeviceFormat {
            rate: wave.nSamplesPerSec,
            channels: usize::from(wave.nChannels),
            sample,
        })
    }

    fn wide(text: PWSTR) -> String {
        // SAFETY: a NUL-terminated string the API allocated; freed by the caller.
        unsafe { text.to_string().unwrap_or_default() }
    }

    /// SAFETY: COM is initialised on this thread.
    unsafe fn friendly_name(device: &IMMDevice) -> String {
        unsafe {
            let Ok(store) = device.OpenPropertyStore(STGM_READ) else {
                return String::new();
            };
            let Ok(mut value) = store.GetValue(&PKEY_Device_FriendlyName) else {
                return String::new();
            };
            let name = match PropVariantToStringAlloc(&value) {
                Ok(text) => {
                    let name = wide(text);
                    CoTaskMemFree(Some(text.0.cast()));
                    name
                }
                Err(_) => String::new(),
            };
            let _ = PropVariantClear(&mut value);
            name
        }
    }

    /// SAFETY: COM is initialised on this thread.
    unsafe fn enumerator() -> Result<IMMDeviceEnumerator, String> {
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| format!("audio devices unavailable: {e}"))
    }

    /// The default device, or the first active one whose name contains `wanted`.
    ///
    /// SAFETY: COM is initialised on this thread.
    unsafe fn find_device(flow: EDataFlow, wanted: Option<&str>) -> Result<IMMDevice, String> {
        unsafe {
            let devices = enumerator()?;
            let Some(wanted) = wanted.map(str::to_lowercase).filter(|w| !w.is_empty()) else {
                return devices
                    .GetDefaultAudioEndpoint(flow, eConsole)
                    .map_err(|e| format!("no default device: {e}"));
            };
            let collection = devices
                .EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)
                .map_err(|e| e.to_string())?;
            for i in 0..collection.GetCount().unwrap_or(0) {
                if let Ok(device) = collection.Item(i) {
                    if friendly_name(&device).to_lowercase().contains(&wanted) {
                        return Ok(device);
                    }
                }
            }
            Err(format!("no active device named like {wanted:?}"))
        }
    }

    /// SAFETY: COM is initialised on this thread; `client` is not initialised yet.
    unsafe fn start_client(
        client: IAudioClient,
        format: DeviceFormat,
        event: Option<HANDLE>,
    ) -> Result<Stream, String> {
        unsafe {
            if let Some(event) = event {
                client.SetEventHandle(event).map_err(|e| e.to_string())?;
            }
            let capture: IAudioCaptureClient = client.GetService().map_err(|e| e.to_string())?;
            client.Start().map_err(|e| e.to_string())?;
            Ok(Stream {
                client,
                capture,
                format,
                event,
            })
        }
    }

    /// The output device's loopback or a microphone, converted by WASAPI to 48 kHz
    /// stereo float when it can, else in the device's own format.
    ///
    /// SAFETY: COM is initialised on this thread.
    unsafe fn open_endpoint(
        kind: SourceKind,
        wanted: Option<&str>,
    ) -> Result<(Stream, String), String> {
        unsafe {
            let flow = if kind == SourceKind::Microphone {
                eCapture
            } else {
                eRender
            };
            let device = find_device(flow, wanted)?;
            let name = friendly_name(&device);
            let base = if kind == SourceKind::Desktop {
                AUDCLNT_STREAMFLAGS_LOOPBACK
            } else {
                0
            };
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| e.to_string())?;
            let wanted_format = float_format(RATE);
            let converted = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                base | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                BUFFER_100NS,
                0,
                &wanted_format,
                None,
            );
            if converted.is_ok() {
                let format = DeviceFormat {
                    rate: RATE,
                    channels: 2,
                    sample: SampleFormat::F32,
                };
                return Ok((start_client(client, format, None)?, name));
            }
            // The device's own mix format, converted here (and resampled by ffmpeg).
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| e.to_string())?;
            let mix = client.GetMixFormat().map_err(|e| e.to_string())?;
            let format = device_format(mix);
            let initialised =
                client.Initialize(AUDCLNT_SHAREMODE_SHARED, base, BUFFER_100NS, 0, mix, None);
            CoTaskMemFree(Some(mix.cast()));
            let format = format?;
            initialised.map_err(|e| format!("cannot open {name}: {e}"))?;
            Ok((start_client(client, format, None)?, name))
        }
    }

    #[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
    struct Activation(Mutex<Option<std::sync::mpsc::Sender<()>>>);

    impl IActivateAudioInterfaceCompletionHandler_Impl for Activation_Impl {
        fn ActivateCompleted(
            &self,
            _operation: Ref<IActivateAudioInterfaceAsyncOperation>,
        ) -> windows::core::Result<()> {
            if let Some(done) = self.0.lock().ok().and_then(|mut d| d.take()) {
                let _ = done.send(());
            }
            Ok(())
        }
    }

    impl IAgileObject_Impl for Activation_Impl {}

    /// League's audio (and its child processes') through process loopback.
    ///
    /// SAFETY: COM is initialised on this thread.
    unsafe fn open_process_loopback(pid: u32) -> Result<Stream, String> {
        unsafe {
            let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
                ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
                Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                    ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                        TargetProcessId: pid,
                        ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                    },
                },
            };
            let mut blob = PROPVARIANT::default();
            {
                let inner = &mut *blob.Anonymous.Anonymous;
                inner.vt = VT_BLOB;
                inner.Anonymous.blob = BLOB {
                    cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                    pBlobData: std::ptr::from_mut(&mut params).cast(),
                };
            }
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let handler: IActivateAudioInterfaceCompletionHandler =
                Activation(Mutex::new(Some(done_tx))).into();
            let operation = ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(std::ptr::from_ref(&blob)),
                &handler,
            )
            .map_err(|e| format!("process loopback unavailable: {e}"))?;
            // The blob points at `params`, which lives until here; nothing to free.
            done_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "process loopback didn't activate".to_owned())?;
            let mut result = HRESULT(0);
            let mut activated = None;
            operation
                .GetActivateResult(&mut result, &mut activated)
                .map_err(|e| e.to_string())?;
            result
                .ok()
                .map_err(|e| format!("process loopback refused: {e}"))?;
            let client: IAudioClient = activated
                .ok_or("process loopback returned nothing")?
                .cast()
                .map_err(|e| e.to_string())?;
            let event =
                CreateEventW(None, false, false, PCWSTR::null()).map_err(|e| e.to_string())?;
            let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM;
            let float = float_format(RATE);
            let (format, sample) = if client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    flags,
                    BUFFER_100NS,
                    0,
                    &float,
                    None,
                )
                .is_ok()
            {
                (float, SampleFormat::F32)
            } else {
                let pcm = pcm16_format(RATE);
                if let Err(e) =
                    client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_100NS, 0, &pcm, None)
                {
                    let _ = CloseHandle(event);
                    return Err(format!("process loopback format refused: {e}"));
                }
                (pcm, SampleFormat::I16)
            };
            let format = DeviceFormat {
                rate: format.nSamplesPerSec,
                channels: usize::from(format.nChannels),
                sample,
            };
            start_client(client, format, Some(event))
        }
    }

    /// Reads every packet the client has. Returns whether any arrived.
    ///
    /// SAFETY: `stream` belongs to this thread.
    unsafe fn drain(
        stream: &Stream,
        origin: Origin,
        timeline: &mut Timeline,
        scratch: &mut Vec<f32>,
        out: &mut Vec<f32>,
    ) -> Result<bool, String> {
        let mut any = false;
        loop {
            let pending =
                unsafe { stream.capture.GetNextPacketSize() }.map_err(|e| e.to_string())?;
            if pending == 0 {
                return Ok(any);
            }
            let mut data = std::ptr::null_mut();
            let (mut frames, mut flags, mut qpc) = (0u32, 0u32, 0u64);
            unsafe {
                stream
                    .capture
                    .GetBuffer(
                        &mut data,
                        &mut frames,
                        &mut flags,
                        None,
                        Some(std::ptr::from_mut(&mut qpc)),
                    )
                    .map_err(|e| e.to_string())?;
            }
            let format = stream.format;
            scratch.clear();
            let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
            if silent || data.is_null() {
                scratch.resize(frames as usize * 2, 0.0);
            } else {
                let size = match format.sample {
                    SampleFormat::F32 | SampleFormat::I32 => 4,
                    SampleFormat::I16 => 2,
                };
                let bytes = unsafe {
                    std::slice::from_raw_parts(data, frames as usize * format.channels * size)
                };
                to_stereo(bytes, format.sample, format.channels, scratch);
            }
            let timed = flags & (AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32) == 0 && qpc != 0;
            timeline.place(timed.then(|| origin.seconds_to(qpc)), scratch, out);
            unsafe { stream.capture.ReleaseBuffer(frames) }.map_err(|e| e.to_string())?;
            any = true;
        }
    }

    /// Process IDs of the running game (names only).
    fn game_pid() -> Option<u32> {
        use std::ffi::OsStr;
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        system
            .processes()
            .values()
            .find(|p| {
                p.name()
                    .eq_ignore_ascii_case(OsStr::new(crate::league::client::GAME_PROCESS))
            })
            .map(|p| p.pid().as_u32())
    }

    pub fn run(
        config: SourceConfig,
        origin: Origin,
        chunks: mpsc::Sender<Vec<u8>>,
        stop: Arc<AtomicBool>,
        ready: oneshot::Sender<Result<u32, String>>,
    ) -> SourceReport {
        // SAFETY: this thread's COM apartment, released at the end.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut report = SourceReport {
            kind: config.kind,
            device: String::new(),
            stats: Default::default(),
            error: None,
        };
        let outcome = capture(&config, origin, &chunks, &stop, ready, &mut report);
        if let Err(error) = outcome {
            tracing::warn!(kind = ?config.kind, %error, "audio capture stopped");
            report.error = Some(error);
        }
        // SAFETY: matches the CoInitializeEx above; every COM object is gone by now.
        unsafe { CoUninitialize() };
        report
    }

    fn capture(
        config: &SourceConfig,
        origin: Origin,
        chunks: &mpsc::Sender<Vec<u8>>,
        stop: &AtomicBool,
        ready: oneshot::Sender<Result<u32, String>>,
        report: &mut SourceReport,
    ) -> Result<(), String> {
        let game = config.kind == SourceKind::Game;
        let mut stream: Option<Stream> = None;
        let rate;
        if game {
            report.device = crate::league::client::GAME_PROCESS.to_owned();
            // Converted to RATE by the engine; it may only start later.
            rate = RATE;
        } else {
            // SAFETY: COM is initialised on this thread.
            match unsafe { open_endpoint(config.kind, config.device.as_deref()) } {
                Ok((opened, name)) => {
                    rate = opened.format.rate;
                    report.device = name;
                    stream = Some(opened);
                }
                Err(error) => {
                    let _ = ready.send(Err(error.clone()));
                    return Err(error);
                }
            }
        }
        let _ = ready.send(Ok(rate));
        tracing::info!(kind = ?config.kind, device = %report.device, rate, "audio source started");

        let mut timeline = Timeline::new(rate);
        let mut scratch = Vec::new();
        let mut out = Vec::new();
        let mut game_target: Option<u32> = None;
        let mut next_check = Instant::now();
        let result = loop {
            if stop.load(Ordering::Relaxed) {
                break Ok(());
            }
            // Game audio: (re)attach to League's process when it appears or changes.
            if game && Instant::now() >= next_check {
                next_check = Instant::now() + GAME_SCAN;
                let pid = game_pid();
                if pid != game_target {
                    stream = None;
                    game_target = pid;
                    if let Some(pid) = pid {
                        // SAFETY: COM is initialised on this thread.
                        match unsafe { open_process_loopback(pid) } {
                            Ok(opened) => {
                                tracing::info!("recording League's audio");
                                stream = Some(opened);
                            }
                            Err(error) => {
                                tracing::warn!(%error, "cannot record League's audio");
                                game_target = None;
                            }
                        }
                    }
                }
            }
            let arrived = match &stream {
                // SAFETY: the stream belongs to this thread.
                Some(active) => {
                    match unsafe { drain(active, origin, &mut timeline, &mut scratch, &mut out) } {
                        Ok(arrived) => arrived,
                        Err(error) => {
                            // Device unplugged or switched, or the game gone: silence until
                            // it's back.
                            tracing::warn!(kind = ?config.kind, %error, "audio device lost; reopening");
                            stream = None;
                            game_target = None;
                            next_check = Instant::now() + REOPEN;
                            false
                        }
                    }
                }
                None => false,
            };
            if !arrived {
                timeline.idle(origin.elapsed_s(), &mut out);
            }
            if !out.is_empty() {
                if !send_chunk(chunks, to_bytes(&out), stop) {
                    break Ok(());
                }
                out.clear();
            }
            if stream.is_none() && !game && Instant::now() >= next_check {
                next_check = Instant::now() + REOPEN;
                // SAFETY: COM is initialised on this thread.
                if let Ok((opened, name)) =
                    unsafe { open_endpoint(config.kind, config.device.as_deref()) }
                {
                    if opened.format.rate == rate {
                        report.device = name;
                        stream = Some(opened);
                    }
                }
            }
            match stream.as_ref().and_then(|s| s.event) {
                // SAFETY: the stream's own event. A timeout just means nothing to read;
                // the idle fill keeps time.
                Some(event) => unsafe {
                    let _ = WaitForSingleObject(event, 20);
                },
                None => std::thread::sleep(POLL),
            }
        };
        drop(stream);
        report.stats = timeline.stats();
        result
    }

    pub fn devices() -> Vec<Device> {
        // SAFETY: COM for this call only.
        unsafe {
            let initialised = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let mut found = Vec::new();
            if let Ok(devices) = enumerator() {
                for (flow, input) in [(eRender, false), (eCapture, true)] {
                    let default = devices
                        .GetDefaultAudioEndpoint(flow, eConsole)
                        .ok()
                        .map(|d| friendly_name(&d));
                    if let Ok(collection) = devices.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) {
                        for i in 0..collection.GetCount().unwrap_or(0) {
                            if let Ok(device) = collection.Item(i) {
                                let name = friendly_name(&device);
                                found.push(Device {
                                    default: default.as_deref() == Some(name.as_str()),
                                    name,
                                    input,
                                });
                            }
                        }
                    }
                }
            }
            if initialised {
                CoUninitialize();
            }
            found
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use tokio::sync::{mpsc, oneshot};

    use super::super::audio::{SourceConfig, SourceReport, TimelineStats};
    use super::{Device, Origin};

    pub fn qpc_100ns() -> i64 {
        0
    }

    pub fn run(
        config: SourceConfig,
        _origin: Origin,
        _chunks: mpsc::Sender<Vec<u8>>,
        _stop: Arc<AtomicBool>,
        ready: oneshot::Sender<Result<u32, String>>,
    ) -> SourceReport {
        let _ = ready.send(Err("audio capture needs Windows".into()));
        SourceReport {
            kind: config.kind,
            device: String::new(),
            stats: TimelineStats::default(),
            error: Some("audio capture needs Windows".into()),
        }
    }

    pub fn devices() -> Vec<Device> {
        Vec::new()
    }
}
