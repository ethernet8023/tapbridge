# Build the APK and its Rust JNI library from source.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (!$env:JAVA_HOME) {
    $java = (Get-Command java -ErrorAction Stop).Source
    $env:JAVA_HOME = Split-Path -Parent (Split-Path -Parent $java)
}
if (!$env:ANDROID_HOME) { $env:ANDROID_HOME = Join-Path $env:LOCALAPPDATA 'Android/Sdk' }
$env:ANDROID_SDK_ROOT = $env:ANDROID_HOME
$env:ANDROID_NDK_HOME = Join-Path $env:ANDROID_HOME 'ndk/27.2.12479018'
Push-Location (Join-Path $root 'android')
try {
    & (Join-Path $env:JAVA_HOME 'bin/java.exe') -classpath gradle/wrapper/gradle-wrapper.jar org.gradle.wrapper.GradleWrapperMain assembleDebug @args
    if ($LASTEXITCODE -ne 0) { throw 'Android build failed' }
} finally { Pop-Location }
