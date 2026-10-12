# Ported from android-nfc-bridge platform/windows/accessory-driver.ps1 (MIT).
# Run by crates/desktop/src/usb_link/windows.rs; UNTESTED in tapbridge.
# Bind only AOA devices to Microsoft's signed inbox drivers. No custom INF,
# certificate import, test signing, or downloaded kernel driver is involved.
param(
 [ValidateSet('Check','Prepare','Elevated')][string]$Mode = 'Check',
 [Parameter(Mandatory=$true)][ValidateSet('2d00','2d01','2d04','2d05')][string]$Product,
 [string]$Serial = ''
)
$ErrorActionPreference = 'Stop'
if ($Mode -eq 'Prepare') {
 try {
  $command = "& '$($PSCommandPath.Replace("'", "''"))' -Mode Elevated -Product '$Product' -Serial '$($Serial.Replace("'", "''"))'; exit `$LASTEXITCODE"
  $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
  $process = Start-Process (Join-Path $PSHOME 'powershell.exe') -Verb RunAs -WindowStyle Hidden -Wait -PassThru -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand',$encoded
  exit $process.ExitCode
 } catch { $_ | Out-String | Set-Content (Join-Path $PSScriptRoot 'accessory-error.txt'); exit 1 }
}
Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.ComponentModel;
public static class InboxDriver {
 [StructLayout(LayoutKind.Sequential)] public struct Device { public uint Size; public Guid Class; public uint DevInst; public IntPtr Reserved; }
 [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct Params {
  public uint Size, Flags, FlagsEx; public IntPtr Window, Callback, Context, Queue, Reserved; public uint Reserved2;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst=260)] public string Path;
 }
 [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct Driver {
  public uint Size, Type; public IntPtr Reserved;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst=256)] public string Description;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst=256)] public string Manufacturer;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst=256)] public string Provider;
  public long Date; public ulong Version;
 }
 [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct Detail {
  public uint Size; public long Date; public uint Offset, Length; public IntPtr Reserved;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst=256)] public string Section;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst=260)] public string Inf;
  [MarshalAs(UnmanagedType.ByValTStr, SizeConst=256)] public string Description;
  public char HardwareId;
 }
 [DllImport("setupapi.dll", SetLastError=true)] static extern IntPtr SetupDiCreateDeviceInfoList(IntPtr guid, IntPtr window);
 [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool SetupDiOpenDeviceInfo(IntPtr set, string id, IntPtr window, uint flags, ref Device device);
 [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool SetupDiGetDeviceInstallParams(IntPtr set, ref Device device, ref Params p);
 [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool SetupDiSetDeviceInstallParams(IntPtr set, ref Device device, ref Params p);
 [DllImport("setupapi.dll", SetLastError=true)] static extern bool SetupDiBuildDriverInfoList(IntPtr set, ref Device device, uint type);
 [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool SetupDiEnumDriverInfo(IntPtr set, ref Device device, uint type, uint index, ref Driver driver);
 [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool SetupDiGetDriverInfoDetail(IntPtr set, ref Device device, ref Driver driver, IntPtr detail, uint size, out uint required);
 [DllImport("setupapi.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool SetupDiSetSelectedDriver(IntPtr set, ref Device device, ref Driver driver);
 [DllImport("newdev.dll", SetLastError=true)] static extern bool DiInstallDevice(IntPtr window, IntPtr set, ref Device device, ref Driver driver, uint flags, out bool reboot);
 [DllImport("setupapi.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool SetupDiSetDeviceRegistryProperty(IntPtr set,ref Device device,uint property,byte[] data,uint length);
 [DllImport("setupapi.dll")] static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);
 [DllImport("setupapi.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr SetupDiGetClassDevs(IntPtr guid,string enumerator,IntPtr window,uint flags);
 [DllImport("setupapi.dll",SetLastError=true)] static extern bool SetupDiEnumDeviceInfo(IntPtr set,uint index,ref Device device);
 [DllImport("setupapi.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool SetupDiGetDeviceInstanceId(IntPtr set,ref Device device,System.Text.StringBuilder id,uint size,out uint required);
 [DllImport("setupapi.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool SetupDiGetDeviceRegistryProperty(IntPtr set,ref Device device,uint property,out uint type,byte[] data,uint size,out uint required);
 public static string[] Present(string product,string serial) {
  var ids=new System.Collections.Generic.List<string>();
  IntPtr set=SetupDiGetClassDevs(IntPtr.Zero,"USB",IntPtr.Zero,6);
  if(set==new IntPtr(-1)) throw new Win32Exception(Marshal.GetLastWin32Error());
  try { for(uint i=0;;i++) {
   var device=new Device {Size=(uint)Marshal.SizeOf(typeof(Device))};
   if(!SetupDiEnumDeviceInfo(set,i,ref device)) { if(Marshal.GetLastWin32Error()!=259) Check(false); break; }
   var id=new System.Text.StringBuilder(1024); uint required;
   Check(SetupDiGetDeviceInstanceId(set,ref device,id,1024,out required));
   string value=id.ToString();
   if(!value.StartsWith("USB\\VID_18D1&PID_"+product,StringComparison.OrdinalIgnoreCase)) continue;
   // Serial matching is done on parents; children have Windows-generated IDs.
   if(value.Split('\\')[1].Length!=17 && value.IndexOf("&MI_00\\",StringComparison.OrdinalIgnoreCase)<0) continue;
   ids.Add(value);
  }} finally {SetupDiDestroyDeviceInfoList(set);}
  if(serial.Length==0) return ids.ToArray();
  string parent=ids.Find(id=>id.EndsWith("\\"+serial,StringComparison.OrdinalIgnoreCase));
  if(parent==null) return new string[0];
  // The Rust client already selected the single physical phone. Refuse an
  // ambiguous driver repair rather than modifying a second accessory.
  if(ids.FindAll(id=>id.Split('\\')[1].Length==17).Count!=1) throw new Exception("Connect only the selected phone during USB driver setup");
  return ids.ToArray();
 }
 public static string Service(string id) {
  IntPtr set=SetupDiCreateDeviceInfoList(IntPtr.Zero,IntPtr.Zero);
  if(set==new IntPtr(-1)) throw new Win32Exception(Marshal.GetLastWin32Error());
  try {
   var device=new Device {Size=(uint)Marshal.SizeOf(typeof(Device))};
   Check(SetupDiOpenDeviceInfo(set,id,IntPtr.Zero,0,ref device));
   uint type,required; byte[] data=new byte[1024];
   if(!SetupDiGetDeviceRegistryProperty(set,ref device,4,out type,data,(uint)data.Length,out required)) return "";
   return System.Text.Encoding.Unicode.GetString(data,0,(int)required).TrimEnd('\0');
  } finally {SetupDiDestroyDeviceInfoList(set);}
 }
 static void Check(bool ok) { if (!ok) throw new Win32Exception(Marshal.GetLastWin32Error()); }
 public static void Bind(string id, string inf, string section) {
  IntPtr set=SetupDiCreateDeviceInfoList(IntPtr.Zero,IntPtr.Zero);
  if (set==new IntPtr(-1)) throw new Win32Exception(Marshal.GetLastWin32Error());
  try {
   var device=new Device {Size=(uint)Marshal.SizeOf(typeof(Device))};
   Check(SetupDiOpenDeviceInfo(set,id,IntPtr.Zero,0,ref device));
   Guid originalClass=device.Class;
   bool changingClass=inf.EndsWith("winusb.inf",StringComparison.OrdinalIgnoreCase);
   bool installed=false;
   try {
   // The class driver list uses the registered device class. Switch to the
   // inbox WinUSB class for selection and restore it if installation fails.
   if (changingClass) { byte[] cls=System.Text.Encoding.Unicode.GetBytes("{88bae032-5a81-49f0-bc3d-a4ff138216d6}\0"); Check(SetupDiSetDeviceRegistryProperty(set,ref device,8,cls,(uint)cls.Length)); device.Class = new Guid("88bae032-5a81-49f0-bc3d-a4ff138216d6"); }
   var p=new Params {Size=(uint)Marshal.SizeOf(typeof(Params))};
   Check(SetupDiGetDeviceInstallParams(set,ref device,ref p));
   p.Flags |= 0x10000; p.FlagsEx |= 0x800; p.Path=inf;
   Check(SetupDiSetDeviceInstallParams(set,ref device,ref p));
   Check(SetupDiBuildDriverInfoList(set,ref device,1));
   for(uint i=0;;i++) {
    var driver=new Driver {Size=(uint)Marshal.SizeOf(typeof(Driver))};
    if(!SetupDiEnumDriverInfo(set,ref device,1,i,ref driver)) throw new Win32Exception(Marshal.GetLastWin32Error());
    IntPtr buffer=Marshal.AllocHGlobal(65536);
    try {
     Marshal.WriteInt32(buffer,Marshal.SizeOf(typeof(Detail)));
     uint required; Check(SetupDiGetDriverInfoDetail(set,ref device,ref driver,buffer,65536,out required));
     var detail=(Detail)Marshal.PtrToStructure(buffer,typeof(Detail));

     if(!String.Equals(detail.Section,section,StringComparison.OrdinalIgnoreCase)) continue;

     Check(SetupDiSetSelectedDriver(set,ref device,ref driver));
     bool reboot; Check(DiInstallDevice(IntPtr.Zero,set,ref device,ref driver,0,out reboot));
     installed=true;
     if(reboot) throw new Exception("USB driver setup requires a reboot"); return;
    } finally {Marshal.FreeHGlobal(buffer);}
   }
   } finally {
    if(changingClass && !installed) {
     byte[] cls=System.Text.Encoding.Unicode.GetBytes(originalClass.ToString("B")+"\0");
     SetupDiSetDeviceRegistryProperty(set,ref device,8,cls,(uint)cls.Length);
    }
   }
  } finally {SetupDiDestroyDeviceInfoList(set);}
 }
}
'@
try {
 $ids = @([InboxDriver]::Present($Product,$Serial))
 $parents = @($ids | Where-Object { ($_ -split '\\')[1].Length -eq 17 })
 if ($parents.Count -ne 1) { throw 'Connect exactly one matching Android accessory during driver setup' }
 $parent = $parents[0]
 $composite = $Product -ne '2d00'
 if ($composite -and [InboxDriver]::Service($parent) -ine 'usbccgp') {
  if ($Mode -eq 'Check') { exit 2 }
  [InboxDriver]::Bind($parent,(Join-Path $env:SystemRoot 'INF\usb.inf'),'Composite.Dev')
  # Re-enumeration replaces vendor-created child devices. The desktop retries
  # discovery before binding the newly enumerated accessory interface.
  exit 0
 }
 $accessory = if ($composite) { $ids | Where-Object { $_ -match '&MI_00\\' } | Select-Object -First 1 } else { $parent }
 if (!$accessory) { throw 'Accessory interface has not enumerated; reconnect the phone' }
 $parameters = "Registry::HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Enum\$accessory\Device Parameters"
 $guids = @((Get-ItemProperty -LiteralPath $parameters -Name DeviceInterfaceGUIDs -ErrorAction SilentlyContinue).DeviceInterfaceGUIDs | Where-Object { $_ })
 if ([InboxDriver]::Service($accessory) -ieq 'WinUSB' -and $guids.Count -gt 0) { exit 0 }
 if ($Mode -eq 'Check') { exit 2 }
 [InboxDriver]::Bind($accessory,(Join-Path $env:SystemRoot 'INF\winusb.inf'),'WINUSB')
 if (!(Test-Path -LiteralPath $parameters)) { New-Item -Path $parameters | Out-Null }
 $guids += '{a8a7b6c5-1c1e-4ec8-bb6d-f747f52797e3}'
 New-ItemProperty -LiteralPath $parameters -Name DeviceInterfaceGUIDs -PropertyType MultiString -Value ($guids | Select-Object -Unique) -Force | Out-Null
 & (Join-Path $env:SystemRoot 'System32\pnputil.exe') /restart-device $accessory | Out-File (Join-Path $PSScriptRoot 'accessory-install.log')
 if ($LASTEXITCODE -ne 0) { throw 'Could not restart the accessory interface; reconnect the phone' }
 exit 0
} catch {
 $_ | Out-String | Set-Content (Join-Path $PSScriptRoot 'accessory-error.txt')
 exit 1
}
