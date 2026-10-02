param([ValidateSet('local','shared')][string]$Mode,[string]$Text,[ValidateSet('enter','escape','page-down','page-up','r','home')][string]$Key,[string]$Capture)
$ErrorActionPreference='Stop'
$taskRoot=(Get-Content ('target/phase-live-'+$Mode+'-root.txt')).Trim()
$taskProcesses=@(Get-CimInstance Win32_Process | Where-Object { $_.Name-eq'rook.exe' -and $_.CommandLine.Contains($taskRoot) -and $_.CommandLine.Contains('tui') })
if($taskProcesses.Count-ne1){throw 'Expected one owned TUI'}
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class PhaseConsole {
 [StructLayout(LayoutKind.Explicit,Size=20)] public struct Input {
  [FieldOffset(0)] public ushort type; [FieldOffset(4)] public int down;
  [FieldOffset(8)] public ushort repeat; [FieldOffset(10)] public ushort vk;
  [FieldOffset(12)] public ushort scan; [FieldOffset(14)] public char ch;
  [FieldOffset(16)] public uint control;
 }
 [StructLayout(LayoutKind.Sequential)] public struct Coord { public short X,Y; }
 [StructLayout(LayoutKind.Sequential)] public struct Rect { public short Left,Top,Right,Bottom; }
 [StructLayout(LayoutKind.Sequential)] public struct Info { public Coord Size,Cursor; public ushort Attributes; public Rect Window; public Coord Maximum; }
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool FreeConsole();
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool AttachConsole(uint pid);
 [DllImport("kernel32.dll",SetLastError=true,CharSet=CharSet.Unicode)] static extern IntPtr CreateFileW(string name,uint access,uint share,IntPtr security,uint mode,uint flags,IntPtr template);
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool CloseHandle(IntPtr handle);
 [DllImport("kernel32.dll",SetLastError=true,CharSet=CharSet.Unicode)] static extern bool WriteConsoleInputW(IntPtr h,Input[] records,uint count,out uint written);
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool GetConsoleScreenBufferInfo(IntPtr h,out Info info);
 [DllImport("kernel32.dll",SetLastError=true,CharSet=CharSet.Unicode)] static extern bool ReadConsoleOutputCharacterW(IntPtr h,StringBuilder text,uint count,Coord start,out uint read);
 public static string Run(uint pid,string text,string key) {
  text=text??""; FreeConsole(); if(!AttachConsole(pid))throw new Win32Exception(Marshal.GetLastWin32Error(),"AttachConsole");
  try {
   if(text.Length>256)throw new Exception("input cap exceeded");
   var events=new System.Collections.Generic.List<Input>();
   foreach(char ch in text)Add(events,(ushort)char.ToUpperInvariant(ch),ch);
   ushort vk=0; char chr='\0';
   switch(key){case "enter":vk=13;chr='\r';break;case "escape":vk=27;chr='\x1b';break;case "page-down":vk=34;break;case "page-up":vk=33;break;case "home":vk=36;break;case "r":vk=82;chr='r';break;}
   if(vk!=0)Add(events,vk,chr);
   if(events.Count>0){
    var input=CreateFileW("CONIN$",0xc0000000,3,IntPtr.Zero,3,0,IntPtr.Zero);
    if(input==new IntPtr(-1))throw new Win32Exception(Marshal.GetLastWin32Error(),"CONIN$");
    try {uint written;if(!WriteConsoleInputW(input,events.ToArray(),(uint)events.Count,out written)||written!=events.Count)throw new Win32Exception(Marshal.GetLastWin32Error(),"WriteConsoleInputW");}
    finally {CloseHandle(input);}
   }
   var output=CreateFileW("CONOUT$",0x80000000,3,IntPtr.Zero,3,0,IntPtr.Zero);
   if(output==new IntPtr(-1))throw new Win32Exception(Marshal.GetLastWin32Error(),"CONOUT$");
   try {
    Info info;if(!GetConsoleScreenBufferInfo(output,out info))throw new Win32Exception(Marshal.GetLastWin32Error(),"console info");
    int width=Math.Min(160,info.Window.Right-info.Window.Left+1),height=Math.Min(60,info.Window.Bottom-info.Window.Top+1);
    var screen=new StringBuilder();
    for(int y=0;y<height;y++){var row=new StringBuilder(width);uint read;if(!ReadConsoleOutputCharacterW(output,row,(uint)width,new Coord{X=info.Window.Left,Y=(short)(info.Window.Top+y)},out read))throw new Win32Exception(Marshal.GetLastWin32Error(),"console row");screen.AppendLine(row.ToString(0,(int)read));}
    return screen.ToString();
   } finally {CloseHandle(output);}
  } finally {FreeConsole();}
 }
 static void Add(System.Collections.Generic.List<Input> events,ushort vk,char ch){var down=new Input{type=1,down=1,repeat=1,vk=vk,ch=ch};var up=down;up.down=0;events.Add(down);events.Add(up);}
}
'@
$taskScreen=[PhaseConsole]::Run([uint32]$taskProcesses[0].ProcessId,$Text,$Key)
if($Capture){[IO.File]::WriteAllText((Join-Path $taskRoot ($Capture+'.txt')),$taskScreen)}
Write-Output $taskScreen
