param([Parameter(Mandatory)][string]$Root,
 [string]$Text,
 [ValidateSet('ctrl-o','ctrl-n','ctrl-q','escape')][string]$Key,
 [ValidateRange(24,160)][int]$Width,
 [ValidatePattern('^[a-z0-9_-]{1,48}$')][string]$Capture)
$ErrorActionPreference='Stop'
$taskRoot=[IO.Path]::GetFullPath($Root)
$taskTarget=[IO.Path]::GetFullPath((Join-Path (Get-Location).Path 'target'))+[IO.Path]::DirectorySeparatorChar
if(!$taskRoot.StartsWith($taskTarget,[StringComparison]::OrdinalIgnoreCase)){throw 'Probe root must be inside this workspace target'}
$taskProcesses=@(Get-CimInstance Win32_Process | Where-Object {$_.Name-eq'terminal_scrollback.exe' -and $_.CommandLine -and $_.CommandLine.Contains($taskRoot)})
if($taskProcesses.Count-ne1){throw 'Expected one owned terminal probe'}
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class ScrollbackConsole {
 [StructLayout(LayoutKind.Explicit,Size=20)] public struct Input {
  [FieldOffset(0)] public ushort type; [FieldOffset(4)] public int down;
  [FieldOffset(8)] public ushort repeat; [FieldOffset(10)] public ushort vk;
  [FieldOffset(12)] public ushort scan; [FieldOffset(14)] public char ch;
  [FieldOffset(16)] public uint control;
 }
 [StructLayout(LayoutKind.Sequential)] public struct Coord { public short X,Y; }
 [StructLayout(LayoutKind.Sequential)] public struct Rect { public short Left,Top,Right,Bottom; }
 [StructLayout(LayoutKind.Sequential)] public struct Info { public Coord Size,Cursor; public ushort Attributes; public Rect Window; public Coord Maximum; }
 public class Snapshot { public string screen,history,resize_error; public int width,height,buffer_width,buffer_rows,cursor_x,cursor_y; }
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool FreeConsole();
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool AttachConsole(uint pid);
 [DllImport("kernel32.dll",SetLastError=true,CharSet=CharSet.Unicode)] static extern IntPtr CreateFileW(string name,uint access,uint share,IntPtr security,uint mode,uint flags,IntPtr template);
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool CloseHandle(IntPtr handle);
 [DllImport("kernel32.dll",SetLastError=true,CharSet=CharSet.Unicode)] static extern bool WriteConsoleInputW(IntPtr handle,Input[] records,uint count,out uint written);
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool GetConsoleScreenBufferInfo(IntPtr handle,out Info info);
 [DllImport("kernel32.dll",SetLastError=true,CharSet=CharSet.Unicode)] static extern bool ReadConsoleOutputCharacterW(IntPtr handle,StringBuilder text,uint count,Coord start,out uint read);
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool SetConsoleWindowInfo(IntPtr handle,bool absolute,ref Rect window);
 [DllImport("kernel32.dll",SetLastError=true)] static extern bool SetConsoleScreenBufferSize(IntPtr handle,Coord size);
 static string Rows(IntPtr handle,int start,int count,int width) {
  if(width>160 || count>1200)throw new Exception("capture admission");
  var output=new StringBuilder();
  for(int y=start;y<start+count;y++){
   var row=new StringBuilder(width);uint read;
   if(!ReadConsoleOutputCharacterW(handle,row,(uint)width,new Coord{X=0,Y=(short)y},out read))throw new Win32Exception(Marshal.GetLastWin32Error(),"console row");
   output.AppendLine(row.ToString(0,(int)read));
  }
  return output.ToString();
 }
 static void Add(System.Collections.Generic.List<Input> events,ushort vk,char ch,uint control) {
  var down=new Input{type=1,down=1,repeat=1,vk=vk,ch=ch,control=control};var up=down;up.down=0;events.Add(down);events.Add(up);
 }
 public static Snapshot Run(uint pid,string text,string key,int width) {
  text=text??"";if(text.Length>256)throw new Exception("input admission");
  FreeConsole();if(!AttachConsole(pid))throw new Win32Exception(Marshal.GetLastWin32Error(),"AttachConsole");
  try {
   var events=new System.Collections.Generic.List<Input>();
   foreach(char ch in text)Add(events,(ushort)char.ToUpperInvariant(ch),ch,0);
   // Explicit letters keep synthetic chords independent of operator layout.
   switch(key){case "escape":Add(events,27,'\x1b',0);break;case "ctrl-o":Add(events,79,'o',8);break;case "ctrl-n":Add(events,78,'n',8);break;case "ctrl-q":Add(events,81,'q',8);break;}
   if(events.Count>0){
    var input=CreateFileW("CONIN$",0xc0000000,3,IntPtr.Zero,3,0,IntPtr.Zero);
    if(input==new IntPtr(-1))throw new Win32Exception(Marshal.GetLastWin32Error(),"CONIN$");
    try{uint written;if(!WriteConsoleInputW(input,events.ToArray(),(uint)events.Count,out written)||written!=events.Count)throw new Win32Exception(Marshal.GetLastWin32Error(),"WriteConsoleInputW");}finally{CloseHandle(input);}
   }
   var handle=CreateFileW("CONOUT$",0xc0000000,3,IntPtr.Zero,3,0,IntPtr.Zero);
   if(handle==new IntPtr(-1))throw new Win32Exception(Marshal.GetLastWin32Error(),"CONOUT$");
   try{
    Info info;if(!GetConsoleScreenBufferInfo(handle,out info))throw new Win32Exception(Marshal.GetLastWin32Error(),"console info");
    string resizeError=null;
    if(width>0){
     var window=info.Window;window.Left=0;window.Right=(short)(width-1);
     if(!SetConsoleWindowInfo(handle,true,ref window) || !SetConsoleScreenBufferSize(handle,new Coord{X=(short)width,Y=info.Size.Y}))resizeError=new Win32Exception(Marshal.GetLastWin32Error()).Message;
     if(!GetConsoleScreenBufferInfo(handle,out info))throw new Win32Exception(Marshal.GetLastWin32Error(),"resized console info");
    }
    int columns=info.Window.Right-info.Window.Left+1,rows=info.Window.Bottom-info.Window.Top+1;
    if(columns>160 || rows>60 || info.Size.X>160)throw new Exception("geometry admission");
    return new Snapshot{screen=Rows(handle,info.Window.Top,rows,columns),history=Rows(handle,0,Math.Min((int)info.Size.Y,1200),info.Size.X),resize_error=resizeError,width=columns,height=rows,buffer_width=info.Size.X,buffer_rows=info.Size.Y,cursor_x=info.Cursor.X,cursor_y=info.Cursor.Y};
   }finally{CloseHandle(handle);}
  }finally{FreeConsole();}
 }
}
'@
$taskSnapshot=[ScrollbackConsole]::Run([uint32]$taskProcesses[0].ProcessId,$Text,$Key,$Width)
$taskJson=$taskSnapshot | ConvertTo-Json -Depth 4
if($Capture){[IO.File]::WriteAllText((Join-Path $taskRoot ($Capture+'.json')),$taskJson)}
Write-Output $taskJson
