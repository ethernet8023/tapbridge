# NixOS VM test for services.tapbridge: the module's wiring against a real
# pcscd, real udev and the real kernel's /dev/uhid.
#
#   nix build .#checks.x86_64-linux.vm -L
#
# The app itself needs a display, so the PC/SC half talks to a stub that
# speaks the same socket protocol (nix/tests/ifd-stub.py); the Rust side of
# that protocol has its own end-to-end test against a real host service. The
# security-key half runs the real driver (cargo's `#[ignore]` live tests, as
# root) against the real kernel with libfido2's fido2-token as the client.
{ pkgs, module, package, tests }:

let
in
pkgs.testers.runNixOSTest {
  name = "tapbridge";

  nodes.machine = { ... }: {
    imports = [ module ];
    services.tapbridge = {
      enable = true;
      inherit package;
      users = [ "alice" ];
      pam.sudo = true;
      pam.su = true;
    };
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
    };
    environment.systemPackages = [ pkgs.opensc pkgs.pcsc-tools pkgs.libfido2 pkgs.python3 pkgs.binutils tests ];
  };

  testScript = ''
    machine.wait_for_unit("multi-user.target")

    with subtest("the module makes alice a socket directory only she and pcscd can use"):
        machine.succeed("stat -c %U:%G:%a /run/tapbridge/alice | grep -x alice:pcscd:2750")

    with subtest("pcscd lists the virtual reader with no USB device"):
        machine.succeed("su alice -c 'python3 ${./tests/ifd-stub.py} /run/tapbridge/alice/ifd.sock &>/tmp/stub.log &'")
        machine.wait_until_succeeds("test -S /run/tapbridge/alice/ifd.sock", timeout=30)
        machine.wait_until_succeeds("su alice -c 'opensc-tool -l' | grep -w tapbridge", timeout=60)

    with subtest("alice (no local session) reaches the card through the shim"):
        # pcscd polls card presence about once a second: wait for the stub to be seen.
        out = machine.wait_until_succeeds("su alice -c \"opensc-tool -r 0 -a\"", timeout=30)
        assert "3b:8f:80:01:80:4f:0c:a0:00:00:03:06:03:00:03:00:00:00:00:68" in out.lower(), out
        code, out = machine.execute("su alice -c \"opensc-tool -r 0 -s 00:a4:04:00:00\" 2>&1")
        print(out)
        print(machine.succeed("tail -n 20 /tmp/stub.log"))
        assert code == 0, out
        assert "SW1=0x90, SW2=0x00" in out, out

    with subtest("the app can find the libraries it loads at run time (wayland, xkbcommon, vulkan)"):
        exe = machine.succeed("sed -n 's/^ExecStart=//p' /etc/systemd/user/tapbridge.service").strip()
        runpath = machine.succeed(f"readelf -d {exe} | grep -E 'RUNPATH|RPATH'")
        for lib in ["wayland", "libxkbcommon", "vulkan-loader"]:
            assert lib in runpath, f"{lib} missing from {runpath}"

    with subtest("udev gives the logged-in user the key's device nodes"):
        print(machine.succeed("ls -l /dev/uhid; udevadm info /dev/uhid"))
        machine.succeed("udevadm info /dev/uhid | grep -E 'TAGS=.*uaccess'")

    with subtest("sudo and su ask pam_u2f first, and the password stays a fallback"):
        for service in ["sudo", "su"]:
            line = machine.succeed(f"grep pam_u2f /etc/pam.d/{service}")
            assert "sufficient" in line, line
            assert "cue" in line, line
        machine.succeed("! grep -q pam_u2f /etc/pam.d/login")

    with subtest("a bare tapbridge is not on PATH (it would run without the unit's environment)"):
        machine.fail("command -v tapbridge")

    with subtest("the app's Register button can find pamu2fcfg"):
        # alice has no running user session here, so read the unit file itself.
        unit = machine.succeed("cat /etc/systemd/user/tapbridge.service")
        line = [l for l in unit.splitlines() if "TAPBRIDGE_PAMU2FCFG" in l][0]
        path = line.split("TAPBRIDGE_PAMU2FCFG=", 1)[1].strip().strip('"')
        assert path.endswith("/bin/pamu2fcfg"), line
        machine.succeed(f"test -x {path}")

    with subtest("libfido2 sees and talks to the virtual key through the real kernel"):
        machine.succeed("tapbridge-tests --ignored --exact security_key::uhid_live_tests::libfido2_reads_get_info_through_the_real_kernel --nocapture")
        machine.succeed("tapbridge-tests --ignored --exact security_key::uhid_live_tests::libfido2_lists_the_virtual_key --nocapture")
  '';
}
