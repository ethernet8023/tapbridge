#!/bin/bash
# Runs the Android PIV setup code against the real PivApplet (the applet the
# flexSecure uses) inside jcardsim: PivAppletSimTest, skipped by default.
# Run inside the flake dev shell: nix develop -c scripts/pivapplet-sim.sh
# Downloads PivApplet + jcardsim into $TMPDIR/pivapplet-sim on first run.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
work="${TMPDIR:-/tmp}/pivapplet-sim"
PIVAPPLET_REV=5cb14a9  # arekinath/PivApplet master, Aug 2022
JCARDSIM=https://repo1.maven.org/maven2/com/klinec/jcardsim/3.0.5.11/jcardsim-3.0.5.11.jar
BCPROV=https://repo1.maven.org/maven2/org/bouncycastle/bcprov-jdk15on/1.70/bcprov-jdk15on-1.70.jar

mkdir -p "$work"
cd "$work"
[[ -f jcardsim.jar ]] || curl -fsSL -o jcardsim.jar "$JCARDSIM"
[[ -f bcprov.jar ]] || curl -fsSL -o bcprov.jar "$BCPROV"
if [[ ! -d PivApplet ]]; then
    git clone -q https://github.com/arekinath/PivApplet
fi
git -C PivApplet checkout -q "$PIVAPPLET_REV"
# The applet's //#if feature switches are comments; compiled as-is every
# feature is on (RSA, EC, P-384, AES + 3DES admin keys, extended length).
rm -rf classes && mkdir classes
"$JAVA_HOME/bin/javac" -nowarn -source 8 -target 8 -cp jcardsim.jar -d classes \
    PivApplet/src/net/cooperi/pivapplet/*.java 2>&1 | grep -v '^warning\|^Note' || true

export PIVAPPLET_SIM_CP="$work/classes:$work/jcardsim.jar:$work/bcprov.jar"
cd "$root/android"
"$JAVA_HOME/bin/java" -classpath gradle/wrapper/gradle-wrapper.jar \
    org.gradle.wrapper.GradleWrapperMain --console=plain \
    testDebugUnitTest --tests 'computer.ethernet.tapbridge.PivAppletSimTest' --rerun "$@"
