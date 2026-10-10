//! JNI calls are serialized against release by the Kotlin connection owner.
use crate::Session;
use jni::{
    JNIEnv,
    objects::{GlobalRef, JByteArray, JClass, JObject, JString, JValue},
    sys::{jboolean, jbyteArray, jint, jlong, jstring},
};
use std::{
    io,
    sync::{Mutex, OnceLock},
};
use tapbridge_core::wire::Frame;

static CONTEXT: OnceLock<GlobalRef> = OnceLock::new();
static INIT: Mutex<()> = Mutex::new(());

fn report(env: &mut JNIEnv, error: impl std::fmt::Display) {
    let _ = env.throw_new("java/io/IOException", error.to_string());
}

// All handles originate from Box::into_raw below. Kotlin holds a shared
// lifecycle lock for every call and an exclusive lock for release.
unsafe fn session<'a>(handle: jlong) -> io::Result<&'a Session> {
    if handle == 0 {
        return Err(io::Error::other("connection closed"));
    }
    Ok(unsafe { &*(handle as *const Session) })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_initialize(
    mut env: JNIEnv,
    _class: JClass,
    context: JObject,
) {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let _guard = INIT.lock().map_err(|_| "JNI context unavailable")?;
        if CONTEXT.get().is_none() {
            let vm = env.get_java_vm()?;
            let global = env.new_global_ref(context)?;
            unsafe {
                ndk_context::initialize_android_context(
                    vm.get_java_vm_pointer().cast(),
                    global.as_obj().as_raw().cast(),
                );
            }
            let _ = CONTEXT.set(global);
        }
        Ok(())
    })();
    if let Err(error) = result {
        report(&mut env, error);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_generateKey(
    mut env: JNIEnv,
    _class: JClass,
) -> jbyteArray {
    match env.byte_array_from_slice(&iroh::SecretKey::generate().to_bytes()) {
        Ok(array) => array.into_raw(),
        Err(error) => {
            report(&mut env, error);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_connect(
    mut env: JNIEnv,
    _class: JClass,
    key: JByteArray,
    hint: JString,
    invitation: JString,
    name: JString,
    push_token: JString,
) -> jlong {
    let result = (|| -> Result<Session, Box<dyn std::error::Error>> {
        let bytes = env.convert_byte_array(key)?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "identity must contain 32 bytes")?;
        let hint: String = env.get_string(&hint)?.into();
        let invitation: String = env.get_string(&invitation)?.into();
        let name: String = env.get_string(&name)?.into();
        let push_token: String = env.get_string(&push_token)?.into();
        let push = if push_token.is_empty() {
            None
        } else {
            Some(push_token)
        };
        Ok(Session::connect(key, &hint, &invitation, &name, push)?)
    })();
    match result {
        Ok(session) => Box::into_raw(Box::new(session)) as jlong,
        Err(error) => {
            report(&mut env, error);
            0
        }
    }
}

/// Host display name from the `Accepted` handshake reply.
#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_hostName(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    match unsafe { session(handle) } {
        Ok(s) => match env.new_string(s.host_name()) {
            Ok(text) => text.into_raw(),
            Err(error) => {
                report(&mut env, error);
                std::ptr::null_mut()
            }
        },
        Err(error) => {
            report(&mut env, error);
            std::ptr::null_mut()
        }
    }
}

/// Hint text actually used for the dial, so Kotlin can cache it for later
/// reconnects instead of re-discovering the host every time.
#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_usedHint(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jstring {
    match unsafe { session(handle) } {
        Ok(s) => match env.new_string(s.used_hint()) {
            Ok(text) => text.into_raw(),
            Err(error) => {
                report(&mut env, error);
                std::ptr::null_mut()
            }
        },
        Err(error) => {
            report(&mut env, error);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_read(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    callbacks: JObject,
) -> jboolean {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let frame = unsafe { session(handle)? }.read()?;
        match frame {
            Frame::Apdu {
                id,
                generation,
                data,
            } => {
                let bytes = env.byte_array_from_slice(&data)?;
                env.call_method(
                    &callbacks,
                    "request",
                    "(JJ[B)V",
                    &[
                        JValue::Long(id as jlong),
                        JValue::Long(generation as jlong),
                        JValue::Object(bytes.as_ref()),
                    ],
                )?;
            }
            Frame::Cancel { id } => {
                env.call_method(&callbacks, "cancel", "(J)V", &[JValue::Long(id as jlong)])?;
            }
            Frame::ReadCard => {
                env.call_method(&callbacks, "readCard", "()V", &[])?;
            }
            Frame::Auth { state } => {
                env.call_method(
                    &callbacks,
                    "authState",
                    "(I)V",
                    &[JValue::Int(state.into())],
                )?;
            }
            Frame::SetupPiv { uid } => {
                let bytes = env.byte_array_from_slice(&uid)?;
                env.call_method(
                    &callbacks,
                    "setupPiv",
                    "([B)V",
                    &[JValue::Object(bytes.as_ref())],
                )?;
            }
            _ => return Err("invalid host frame".into()),
        }
        Ok(())
    })();
    match result {
        Ok(()) => 1,
        Err(error) => {
            report(&mut env, error);
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_card(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    generation: jlong,
    atr: JByteArray,
    max_apdu: jint,
    uid: JByteArray,
) {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let atr = env.convert_byte_array(atr)?;
        let uid = env.convert_byte_array(uid)?;
        unsafe { session(handle)? }.send(&Frame::Card {
            generation: generation as u64,
            atr,
            max_apdu: max_apdu as u32,
            uid,
        })?;
        Ok(())
    })();
    if let Err(error) = result {
        report(&mut env, error);
    }
}

/// The PIV setup the host asked for (`SetupPiv`) finished.
#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_pivSetupDone(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    ok: jboolean,
) {
    let result =
        unsafe { session(handle) }.and_then(|s| s.send(&Frame::PivSetupDone { ok: ok != 0 }));
    if let Err(error) = result {
        report(&mut env, error);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_response(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    id: jlong,
    generation: jlong,
    data: JByteArray,
) {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let data = env.convert_byte_array(data)?;
        unsafe { session(handle)? }.send(&Frame::Response {
            id: id as u64,
            generation: generation as u64,
            data,
        })?;
        Ok(())
    })();
    if let Err(error) = result {
        report(&mut env, error);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_failed(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    id: jlong,
    generation: jlong,
) {
    let result = unsafe { session(handle) }.and_then(|s| {
        s.send(&Frame::Failed {
            id: id as u64,
            generation: generation as u64,
        })
    });
    if let Err(error) = result {
        report(&mut env, error);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_close(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    match unsafe { session(handle) } {
        Ok(s) => s.close(),
        Err(error) => report(&mut env, error),
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_computer_ethernet_tapbridge_NativeBridge_release(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        drop(unsafe { Box::from_raw(handle as *mut Session) });
    }
}
