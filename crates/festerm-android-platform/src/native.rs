use crate::{AuthCallback, AuthRequests, AuthResult, Input, InputQueue, MAX_INPUT_BYTES};
use jni_legacy::{
    objects::{GlobalRef, JByteArray, JObject, JString, JValue},
    sys::{jfloat, jint, jlong},
    JNIEnv, JavaVM,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use zeroize::Zeroize;

type Wake = Arc<dyn Fn() + Send + Sync>;
type LifecycleCallback = Arc<dyn Fn(i32) + Send + Sync>;
type SpeechCallback = Arc<dyn Fn(i64, SpeechEvent<'_>) + Send + Sync>;

pub enum SpeechEvent<'a> {
    Status {
        status: i32,
        text: &'a str,
    },
    /// The recognizer's RMS estimate in dB, not PCM samples.
    Level(f32),
}

pub enum PickError {
    Cancelled,
    Unavailable,
}

struct Bridge {
    vm: JavaVM,
    activity: GlobalRef,
    wake: Mutex<Option<Wake>>,
    input: Mutex<InputQueue>,
    input_epoch: AtomicU64,
    input_sequence: AtomicU64,
    insets: Mutex<Insets>,
    hinge: Mutex<Option<(f32, f32)>>,
    lifecycle: Mutex<Option<LifecycleCallback>>,
    authentication: Mutex<AuthRequests>,
    speech: Mutex<Option<SpeechCallback>>,
    picked: Mutex<Option<Result<PathBuf, PickError>>>,
}

static BRIDGE: Mutex<Option<Arc<Bridge>>> = Mutex::new(None);

fn bridge() -> Result<Arc<Bridge>, &'static str> {
    BRIDGE
        .lock()
        .map_err(|_| "Android bridge is unavailable")?
        .clone()
        .ok_or("Android activity is unavailable")
}

impl Bridge {
    fn wake(&self) {
        let wake = self
            .wake
            .lock()
            .expect("Android wake state poisoned")
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    fn call<T>(
        &self,
        operation: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> jni_legacy::errors::Result<T>,
    ) -> Result<T, &'static str> {
        let mut env = self
            .vm
            .attach_current_thread()
            .map_err(|_| "Android VM is unavailable")?;
        let result = env.with_local_frame(32, |env| operation(env, self.activity.as_obj()));
        result.map_err(|_| {
            // Exception messages may contain user paths or credentials.
            let _ = env.exception_clear();
            eprintln!("fesTerm: Android native service call failed");
            "Android native service is unavailable"
        })
    }

    /// Best effort: the prompt may already be gone.
    fn cancel_authentication(&self, token: u64) {
        let _ = self.call(|env, activity| {
            env.call_method(
                activity,
                "cancelAuthentication",
                "(J)V",
                &[JValue::Long(token as i64)],
            )?;
            Ok(())
        });
    }
}

pub fn initialize(
    app: &winit::platform::android::activity::AndroidApp,
) -> Result<(), &'static str> {
    let context = ndk_context::android_context();
    // The verifier retains the application context, not this Activity.
    let tls_vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
    tls_vm
        .attach_current_thread(|env| -> jni::errors::Result<()> {
            use jni::{jni_sig, jni_str, objects::JObject};
            let raw = context.context() as jni::sys::jobject;
            let activity = unsafe { env.as_cast_raw::<JObject>(&raw)? };
            let application = env
                .call_method(
                    activity,
                    jni_str!("getApplicationContext"),
                    jni_sig!(() -> android.content.Context),
                    &[],
                )?
                .l()?;
            rustls_platform_verifier::android::init_with_env(env, application)
        })
        .map_err(|_| "Android TLS verifier initialization failed")?;
    // android-activity owns these references for this activity's lifetime.
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) }
        .map_err(|_| "Android VM is unavailable")?;
    let activity = {
        let env = vm
            .attach_current_thread()
            .map_err(|_| "Android VM is unavailable")?;
        let object = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        env.new_global_ref(&object)
            .map_err(|_| "Android activity is unavailable")?
    };
    *BRIDGE.lock().map_err(|_| "Android bridge is unavailable")? = Some(Arc::new(Bridge {
        vm,
        activity,
        wake: Mutex::new(None),
        input: Mutex::new(InputQueue::default()),
        input_epoch: AtomicU64::new(0),
        input_sequence: AtomicU64::new(0),
        insets: Mutex::new(Insets::default()),
        hinge: Mutex::new(None),
        lifecycle: Mutex::new(None),
        authentication: Mutex::new(AuthRequests::default()),
        speech: Mutex::new(None),
        picked: Mutex::new(None),
    }));
    Ok(())
}

pub fn shutdown() {
    if let Ok(mut slot) = BRIDGE.lock() {
        if let Some(bridge) = slot.take() {
            let done = bridge
                .authentication
                .lock()
                .expect("auth state poisoned")
                .take_pending();
            if let Some(done) = done {
                done(AuthResult::Cancelled);
            }
        }
    }
}

pub fn set_repaint(wake: Wake) -> Result<(), &'static str> {
    *bridge()?
        .wake
        .lock()
        .map_err(|_| "Android wake state is unavailable")? = Some(wake);
    Ok(())
}

pub fn request_repaint() -> Result<(), &'static str> {
    bridge()?.wake();
    Ok(())
}

pub fn take_input() -> Result<Vec<Input>, &'static str> {
    let bridge = bridge()?;
    let (result, sequence) = {
        let mut input = bridge
            .input
            .lock()
            .map_err(|_| "Android input is unavailable")?;
        let result = input.take();
        let sequence = match &result {
            Ok(batch) => batch.last().map_or(0, |event| event.sequence),
            Err(_) => bridge.input_sequence.load(Ordering::Acquire),
        };
        (result, sequence)
    };
    if bridge
        .input
        .lock()
        .map_err(|_| "Android input is unavailable")?
        .has_pending()
    {
        bridge.wake();
    }
    if sequence != 0 {
        bridge.call(|env, activity| {
            env.call_method(
                activity,
                "inputConsumed",
                "(J)V",
                &[JValue::Long(sequence as i64)],
            )?;
            Ok(())
        })?;
    }
    result
}

pub fn input_epoch() -> u64 {
    bridge()
        .map(|bridge| bridge.input_epoch.load(Ordering::Acquire))
        .unwrap_or(0)
}

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Insets {
    pub occlusion: f32,
    pub top: f32,
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
}

pub fn insets() -> Insets {
    bridge()
        .ok()
        .and_then(|bridge| bridge.insets.lock().ok().map(|v| *v))
        .unwrap_or_default()
}

/// The vertical fold or hinge crossing the window, as `(left, right)`
/// fractions of the window width.
pub fn hinge() -> Option<(f32, f32)> {
    bridge()
        .ok()
        .and_then(|bridge| bridge.hinge.lock().ok().and_then(|v| *v))
}

pub fn occlusion() -> f32 {
    insets().occlusion
}

pub fn storage_directory() -> Result<PathBuf, &'static str> {
    bridge()?.call(|env, activity| {
        let value = env
            .call_method(activity, "privateDirectory", "()Ljava/lang/String;", &[])?
            .l()?;
        let text: String = env.get_string(&JString::from(value))?.into();
        Ok(PathBuf::from(text))
    })
}

pub fn keyboard_mode(mode: i32) -> Result<(), &'static str> {
    bridge()?.call(|env, activity| {
        env.call_method(activity, "keyboardMode", "(I)V", &[JValue::Int(mode)])?;
        Ok(())
    })
}

pub fn field_document(
    id: u64,
    text: &str,
    start: usize,
    end: usize,
    secure: bool,
    multiline: bool,
) -> Result<(), &'static str> {
    bridge()?.call(|env, activity| {
        let text = env.new_string(text)?;
        env.call_method(
            activity,
            "fieldDocument",
            "(JLjava/lang/String;IIZZ)V",
            &[
                JValue::Long(id as i64),
                JValue::Object(&text),
                JValue::Int(start as i32),
                JValue::Int(end as i32),
                JValue::Bool(secure.into()),
                JValue::Bool(multiline.into()),
            ],
        )?;
        Ok(())
    })
}

pub fn input_click() -> Result<(), &'static str> {
    bridge()?.call(|env, activity| {
        env.call_method(activity, "inputClick", "()V", &[])?;
        Ok(())
    })
}

pub fn clipboard_text() -> Result<Option<String>, &'static str> {
    bridge()?.call(|env, activity| {
        let value = env
            .call_method(activity, "clipboardText", "()Ljava/lang/String;", &[])?
            .l()?;
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(env.get_string(&JString::from(value))?.into()))
    })
}

pub fn set_clipboard_text(text: &str) -> Result<(), &'static str> {
    bridge()?.call(|env, activity| {
        let text = env.new_string(text)?;
        env.call_method(
            activity,
            "copyText",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&text)],
        )?;
        Ok(())
    })
}

pub fn open_url(url: &str) -> Result<(), &'static str> {
    if !url.starts_with("https://") || url.len() > 8192 || url.chars().any(char::is_control) {
        return Err("Only HTTPS browser links are allowed");
    }
    bridge()?.call(|env, activity| {
        let url = env.new_string(url)?;
        env.call_method(
            activity,
            "openBrowser",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&url)],
        )?;
        Ok(())
    })
}

pub fn authentication_available() -> bool {
    bridge()
        .and_then(|bridge| {
            bridge.call(|env, activity| {
                env.call_method(activity, "canAuthenticate", "()Z", &[])?
                    .z()
            })
        })
        .unwrap_or(false)
}

/// Starts the system prompt and returns the request's token. The newest request
/// wins; a superseded prompt is cancelled and completed as cancelled.
pub fn authenticate(passcode_only: bool, reason: &str, done: AuthCallback) -> u64 {
    let Ok(bridge) = bridge() else {
        done(AuthResult::Unavailable);
        return 0;
    };
    let (token, superseded) = bridge
        .authentication
        .lock()
        .expect("auth state poisoned")
        .begin(done);
    if let Some((old, callback)) = superseded {
        bridge.cancel_authentication(old);
        callback(AuthResult::Cancelled);
    }
    let started = bridge.call(|env, activity| {
        let reason = env.new_string(reason)?;
        env.call_method(
            activity,
            "authenticateOwner",
            "(JZLjava/lang/String;)V",
            &[
                JValue::Long(token as i64),
                JValue::Bool(u8::from(passcode_only)),
                JValue::Object(&reason),
            ],
        )?;
        Ok(())
    });
    if started.is_err() {
        let done = bridge
            .authentication
            .lock()
            .expect("auth state poisoned")
            .take(token);
        if let Some(done) = done {
            done(AuthResult::Unavailable);
        }
    }
    token
}

/// Retires one request: its callback is dropped and only its own prompt is
/// dismissed. A request that already completed or was superseded is untouched.
pub fn cancel_authentication(token: u64) {
    let Ok(bridge) = bridge() else {
        return;
    };
    let pending = bridge
        .authentication
        .lock()
        .expect("auth state poisoned")
        .take(token);
    if pending.is_some() {
        bridge.cancel_authentication(token);
    }
}

pub fn set_lifecycle(callback: LifecycleCallback) -> Result<(), &'static str> {
    *bridge()?
        .lifecycle
        .lock()
        .map_err(|_| "Android lifecycle is unavailable")? = Some(callback);
    Ok(())
}

pub fn privacy(enabled: bool, cover: bool, hide_preview: bool) -> Result<(), &'static str> {
    bridge()?.call(|env, activity| {
        env.call_method(
            activity,
            "privacy",
            "(ZZZ)V",
            &[
                JValue::Bool(enabled.into()),
                JValue::Bool(cover.into()),
                JValue::Bool(hide_preview.into()),
            ],
        )?;
        Ok(())
    })
}

pub fn pick_document() -> Result<(), &'static str> {
    bridge()?.call(|env, activity| {
        env.call_method(activity, "pickDocument", "()V", &[])?;
        Ok(())
    })
}

pub fn take_picked_document() -> Option<Result<PathBuf, PickError>> {
    bridge().ok()?.picked.lock().ok()?.take()
}

pub fn set_speech_callback(callback: SpeechCallback) -> Result<(), &'static str> {
    *bridge()?
        .speech
        .lock()
        .map_err(|_| "Android speech is unavailable")? = Some(callback);
    Ok(())
}

pub fn speech_command(generation: u64, command: i32) -> Result<(), &'static str> {
    bridge()?.call(|env, activity| {
        env.call_method(
            activity,
            "speechCommand",
            "(JI)V",
            &[JValue::Long(generation as i64), JValue::Int(command)],
        )?;
        Ok(())
    })
}

pub fn secret_read(reference: &str) -> Result<Option<Vec<u8>>, &'static str> {
    bridge()?.call(|env, activity| {
        let reference = env.new_string(reference)?;
        let value = env
            .call_method(
                activity,
                "secretRead",
                "(Ljava/lang/String;)[B",
                &[JValue::Object(&reference)],
            )?
            .l()?;
        if value.is_null() {
            return Ok(None);
        }
        let array = JByteArray::from(value);
        let bytes = env.convert_byte_array(&array)?;
        // Wipe the transient JVM copy as well as the Rust owner's allocation.
        let zeros = vec![0_i8; bytes.len()];
        env.set_byte_array_region(&array, 0, &zeros)?;
        Ok(Some(bytes))
    })
}

pub fn secret_write(reference: &str, bytes: &[u8], replace: bool) -> Result<bool, &'static str> {
    bridge()?.call(|env, activity| {
        let reference = env.new_string(reference)?;
        let array = env.byte_array_from_slice(bytes)?;
        let result = env
            .call_method(
                activity,
                "secretWrite",
                "(Ljava/lang/String;[BZ)Z",
                &[
                    JValue::Object(&reference),
                    JValue::Object(&array),
                    JValue::Bool(replace.into()),
                ],
            )
            .and_then(|value| value.z());
        if result.is_err() {
            // Further JNI calls with a pending exception abort the process
            // (for example, a Keystore ProviderException); `call` reports it.
            let _ = env.exception_clear();
        }
        // Java also wipes in finally, including encryption failures.
        let _ = env.set_byte_array_region(&array, 0, &vec![0_i8; bytes.len()]);
        result
    })
}

pub fn secret_delete(reference: &str) -> Result<bool, &'static str> {
    bridge()?.call(|env, activity| {
        let reference = env.new_string(reference)?;
        env.call_method(
            activity,
            "secretDelete",
            "(Ljava/lang/String;)Z",
            &[JValue::Object(&reference)],
        )?
        .z()
    })
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_fes_festerm_FestermActivity_nativeInput(
    mut env: JNIEnv,
    _activity: JObject,
    target: jlong,
    sequence: jlong,
    kind: jint,
    text: JString,
    start: jint,
    end: jint,
) {
    let Ok(bridge) = bridge() else { return };
    let value = env.get_string(&text).map(String::from);
    let mut queue = bridge.input.lock().expect("input state poisoned");
    bridge
        .input_sequence
        .store(sequence as u64, Ordering::Release);
    match value {
        Ok(text) if text.len() <= MAX_INPUT_BYTES && sequence > 0 && start >= 0 && end >= start => {
            queue.push(Input {
                target: target as u64,
                sequence: sequence as u64,
                kind,
                text,
                start: start as usize,
                end: end as usize,
            });
        }
        _ => {
            let _ = env.exception_clear();
            queue.reject();
        }
    }
    drop(queue);
    bridge.wake();
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_fes_festerm_FestermActivity_nativeInsets(
    _env: JNIEnv,
    _activity: JObject,
    fraction: jfloat,
    top: jfloat,
    left: jfloat,
    bottom: jfloat,
    right: jfloat,
) {
    if let Ok(bridge) = bridge() {
        let bounded = |value: f32| {
            if value.is_finite() {
                value.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let next = Insets {
            occlusion: bounded(fraction),
            top: bounded(top),
            left: bounded(left),
            bottom: bounded(bottom),
            right: bounded(right),
        };
        let changed = {
            let mut current = bridge.insets.lock().expect("inset state poisoned");
            let changed = *current != next;
            *current = next;
            changed
        };
        if changed {
            bridge.wake();
        }
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_fes_festerm_FestermActivity_nativeHinge(
    _env: JNIEnv,
    _activity: JObject,
    left: jfloat,
    right: jfloat,
) {
    if let Ok(bridge) = bridge() {
        let next = crate::hinge_span(left, right);
        let changed = {
            let mut current = bridge.hinge.lock().expect("hinge state poisoned");
            let changed = *current != next;
            *current = next;
            changed
        };
        if changed {
            bridge.wake();
        }
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_fes_festerm_FestermActivity_nativeLifecycle(
    _env: JNIEnv,
    _activity: JObject,
    event: jint,
) {
    if let Ok(bridge) = bridge() {
        if event != 1 {
            bridge.input_epoch.fetch_add(1, Ordering::AcqRel);
            bridge.input.lock().expect("input state poisoned").clear();
        }
        let callback = bridge
            .lifecycle
            .lock()
            .expect("lifecycle state poisoned")
            .clone();
        if let Some(callback) = callback {
            callback(event);
        }
        bridge.wake();
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_fes_festerm_FestermActivity_nativeAuthenticated(
    _env: JNIEnv,
    _activity: JObject,
    token: jlong,
    result: jint,
) {
    if let Ok(bridge) = bridge() {
        // A reply for a retired request finds no matching callback.
        let done = bridge
            .authentication
            .lock()
            .expect("auth state poisoned")
            .take(token as u64);
        if let Some(done) = done {
            done(AuthResult::from_code(result));
        }
        bridge.wake();
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_fes_festerm_FestermActivity_nativePicked(
    mut env: JNIEnv,
    _activity: JObject,
    path: JString,
) {
    if let Ok(bridge) = bridge() {
        let result = env
            .get_string(&path)
            .map(String::from)
            .map_err(|_| PickError::Unavailable)
            .and_then(|path| {
                if path.is_empty() {
                    Err(PickError::Cancelled)
                } else {
                    Ok(PathBuf::from(path))
                }
            });
        let _ = env.exception_clear();
        *bridge.picked.lock().expect("picker state poisoned") = Some(result);
        bridge.wake();
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_fes_festerm_FestermActivity_nativeSpeech(
    mut env: JNIEnv,
    _activity: JObject,
    generation: jlong,
    status: jint,
    text: JString,
) {
    if let Ok(bridge) = bridge() {
        let callback = bridge.speech.lock().expect("speech state poisoned").clone();
        if let Some(callback) = callback {
            match env.get_string(&text).map(String::from) {
                Ok(mut text) => {
                    callback(
                        generation,
                        SpeechEvent::Status {
                            status,
                            text: &text,
                        },
                    );
                    text.zeroize();
                }
                Err(_) => {
                    let _ = env.exception_clear();
                    callback(
                        generation,
                        SpeechEvent::Status {
                            status: -1,
                            text: "",
                        },
                    );
                }
            }
        }
        bridge.wake();
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_fes_festerm_FestermActivity_nativeSpeechLevel(
    _env: JNIEnv,
    _activity: JObject,
    generation: jlong,
    level: jfloat,
) {
    if let Ok(bridge) = bridge() {
        let callback = bridge.speech.lock().expect("speech state poisoned").clone();
        if let Some(callback) = callback {
            callback(generation, SpeechEvent::Level(level));
        }
    }
}

/// AccessKit's Android adapter, injected into the activity's dedicated
/// full-size accessibility host view (ADR 0075). Not `Send`: it lives on the
/// native loop thread that publishes egui frames.
pub struct AccessibilityAdapter(accesskit_android::InjectingAdapter);

impl AccessibilityAdapter {
    pub fn new(
        activation: impl accesskit::ActivationHandler + Send + 'static,
        action: impl accesskit::ActionHandler + Send + 'static,
    ) -> Result<Self, &'static str> {
        let bridge = bridge()?;
        // The adapter reads this thread's JNIEnv on every update.
        let mut env = bridge
            .vm
            .attach_current_thread_permanently()
            .map_err(|_| "Android VM is unavailable")?;
        let host = env
            .call_method(
                bridge.activity.as_obj(),
                "accessibilityHost",
                "()Landroid/view/View;",
                &[],
            )
            .and_then(|value| value.l())
            .map_err(|_| {
                let _ = env.exception_clear();
                "Android accessibility host is unavailable"
            })?;
        if host.is_null() {
            return Err("Android accessibility host is unavailable");
        }
        Ok(Self(accesskit_android::InjectingAdapter::new(
            &mut env, &host, activation, action,
        )))
    }

    pub fn update_if_active(&mut self, update: impl FnOnce() -> accesskit::TreeUpdate) {
        self.0.update_if_active(update);
    }
}
