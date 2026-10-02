# R8 rules for release (minify + shrink).

# JNI: Rust looks these methods up by name via call_method / GetMethodID;
# renaming would break every bridge call.
-keep class computer.ethernet.tapbridge.NativeBridge { *; }
-keep interface computer.ethernet.tapbridge.NativeBridge$Callbacks { *; }

# The lifecycle classes Kotlin passes into JNI as callback objects.
-keep class computer.ethernet.tapbridge.NativeGate { *; }
-keep class computer.ethernet.tapbridge.NfcCardSession { *; }

# Firebase / FCM
-keep class com.google.firebase.messaging.FirebaseMessagingService { *; }
-keep class com.google.firebase.messaging.RemoteMessage { *; }

# Keep the wake receiver's service entry point.
-keep class computer.ethernet.tapbridge.WakeService { *; }

# ML Kit barcode scanning (bundled model, JNI-backed)
-keep class com.google.mlkit.vision.** { *; }

# Compose/Kotlin metadata is handled by default rules.
