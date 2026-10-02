# Reproduces a real Windows WebAuthn ceremony (GetAssertion) from a visible
# WinForms window, for checking crates/desktop/src/prompt_watch.rs: the app log
# should show 'wake: WebAuthn ceremony started'. Cancel the dialog to finish.
# Measured on build 28120: the dialog is CredentialUIBroker.exe, class
# 'Credential Dialog Xaml Host'; the WebAuthN/Operational log records event
# 1003 (GetAssertion started) or 1000 (MakeCredential started).

Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class WA {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct CLIENT_DATA {
        public uint dwVersion;
        public uint cbClientDataJSON;
        public IntPtr pbClientDataJSON;
        [MarshalAs(UnmanagedType.LPWStr)] public string pwszHashAlgId;
    }
    [DllImport("webauthn.dll", CharSet = CharSet.Unicode)]
    public static extern int WebAuthNAuthenticatorGetAssertion(IntPtr hWnd, string rpId, ref CLIENT_DATA cd, IntPtr options, out IntPtr assertion);
    public static int Run(IntPtr hwnd) {
        byte[] json = System.Text.Encoding.UTF8.GetBytes("{\"type\":\"webauthn.get\",\"challenge\":\"AAAAAAAAAAAAAAAAAAAAAA\",\"origin\":\"https://example.com\"}");
        IntPtr p = Marshal.AllocHGlobal(json.Length);
        Marshal.Copy(json, 0, p, json.Length);
        var cd = new CLIENT_DATA { dwVersion = 1, cbClientDataJSON = (uint)json.Length, pbClientDataJSON = p, pwszHashAlgId = "SHA-256" };
        IntPtr a;
        return WebAuthNAuthenticatorGetAssertion(hwnd, "example.com", ref cd, IntPtr.Zero, out a);
    }
}
'@
$form = New-Object System.Windows.Forms.Form
$form.Text = 'tapbridge webauthn probe'
$form.Add_Shown({
    $hr = [WA]::Run($form.Handle)
    Write-Host ("WebAuthNAuthenticatorGetAssertion hr=0x{0:X8} (0x800704C7 = cancelled)" -f $hr)
    $form.Close()
})
[System.Windows.Forms.Application]::Run($form)
