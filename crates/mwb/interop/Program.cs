// MWB interop harness. Commands:
//   vectors                                   -> JSON test vectors on stdout
//   decode-stream <key> <hex>                 -> decrypt + parse a Rust-produced stream
//   decode-clip <hex>                         -> decompress Rust clipboard payload
//   peer serve|connect <host> <baseport> <key> <name> <id> <script> <seconds> [clip]
//        a live MWB peer (MainTCPRoutine port) printing JSON lines.
//        script: "control" (send heartbeat/mouse/keys/clipboard/hide to the other side) or "listen".
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Security.Cryptography;
using System.Text.Json;
using System.Threading;
using MouseWithoutBorders.Core;

internal static class Program
{
    private static string Hex(byte[] b) => Convert.ToHexString(b).ToLowerInvariant();

    private static readonly object OutLock = new();

    private static void Emit(object o)
    {
        lock (OutLock)
        {
            Console.Out.WriteLine(JsonSerializer.Serialize(o));
            Console.Out.Flush();
        }
    }

    private static int Main(string[] args)
    {
        Encryption.InitEncryption();
        switch (args.FirstOrDefault())
        {
            case "vectors": Vectors(); return 0;
            case "decode-stream": DecodeStream(args[1], args[2]); return 0;
            case "decode-clip": Console.WriteLine(JsonSerializer.Serialize(new { text = ClipText.Decompress(Convert.FromHexString(args[1])) })); return 0;
            case "peer": return RunPeer(args);
            default: Console.Error.WriteLine("usage: vectors | decode-stream | decode-clip | peer"); return 2;
        }
    }

    private static readonly string[] Keys =
    {
        "abcdEFGH1234!@#$",
        "aB3$cD4%eF5^gH6&",
        "MyVeryLongSecurityKeyThatIsMoreThan32Characters!!",
        "x",
        "Ünïcödé-key-1234中",
    };

    private static Dictionary<string, DATA> SamplePackets()
    {
        var d = new Dictionary<string, DATA>();

        var m = new DATA { Type = PackageType.Mouse, Id = 0x01020304, Src = (ID)0x11223344, Des = ID.ALL };
        m.Md.X = 12345; m.Md.Y = -54321; m.Md.WheelDelta = 120; m.Md.dwFlags = 0x20A;
        d["mouse"] = m;

        var k = new DATA { Type = PackageType.Keyboard, Id = 7, Src = (ID)5, Des = (ID)6, DateTime = 0x0102030405060708 };
        k.Kd.wVk = 0x41; k.Kd.dwFlags = 0x81;
        d["keyboard"] = k;

        var h = new DATA { Type = PackageType.Hello, Id = 9, Src = (ID)5, Des = ID.ALL };
        h.MachineName = "MAC-MINI";
        d["hello"] = h;

        var mx = new DATA { Type = PackageType.Matrix | PackageType.MatrixSwapFlag | PackageType.MatrixTwoRowFlag, Src = (ID)4, Des = ID.ALL };
        mx.MachineName = "WIN-PC";
        d["matrix"] = mx;

        byte[] buf = Enumerable.Range(0, 64).Select(i => (byte)(i * 7 + 3)).ToArray();
        var hs = new DATA(buf);
        hs.MachineName = "HOSTX";
        hs.Type = PackageType.Handshake;
        d["handshake"] = hs;

        var ack = new DATA(hs.Bytes);
        ack.Type = PackageType.HandshakeAck;
        ack.Src = ID.NONE;
        ack.MachineName = "REPLIER";
        ack.Machine1 = ~ack.Machine1; ack.Machine2 = ~ack.Machine2; ack.Machine3 = ~ack.Machine3; ack.Machine4 = ~ack.Machine4;
        d["handshake_ack"] = ack;

        var nm = new DATA { Type = PackageType.NextMachine, Des = (ID)77 };
        nm.Md.X = 100; nm.Md.Y = 200; nm.Md.WheelDelta = 0x12345;
        d["next_machine"] = nm;

        var bye = new DATA { Type = PackageType.ByeBye, Des = ID.ALL };
        bye.MachineName = "IGNORED-SMALL";
        d["byebye"] = bye;
        return d;
    }

    private static void Vectors()
    {
        var hashes = Keys.Select(k => new { key = k, hash = Encryption.Get24BitHash(k) }).ToList();

        Encryption.MyKey = Keys[0];
        Encryption.MagicNumber = Encryption.Get24BitHash(Keys[0]);
        byte[] salt = Enumerable.Range(1, 16).Select(i => (byte)i).ToArray();
        string derived = Hex(Encryption.GenLegalKey(salt));

        var packets = new List<object>();
        foreach (var (name, p) in SamplePackets())
        {
            byte[] bytes = p.Bytes;
            byte[] stamped = (byte[])bytes.Clone();
            Wire.StampForSend(stamped);
            byte[] check = new byte[64];
            Array.Copy(stamped, check, stamped.Length);
            bool valid = Wire.ProcessReceivedDataEx(check);
            packets.Add(new { name, bytes = Hex(bytes), stamped = Hex(stamped), valid });
        }

        // Deterministic encrypted stream: header 01..20, first block a0..af, then mouse + hello.
        byte[] header = Enumerable.Range(1, 32).Select(i => (byte)i).ToArray();
        byte[] first = Enumerable.Range(0, 16).Select(i => (byte)(0xA0 + i)).ToArray();
        var ms = new MemoryStream();
        var enc = Encryption.GetEncryptedStream(ms, header);
        Common.SendOrReceiveARandomDataBlockPerInitialIV(enc, true, first);
        var sp = SamplePackets();
        Wire.TcpSendData(enc, sp["mouse"].Bytes);
        Wire.TcpSendData(enc, sp["hello"].Bytes);
        enc.Flush();
        string stream = Hex(ms.ToArray());

        string clipText = "Hello from C# ✓ ünïcode\r\nline2";
        byte[] clip = ClipText.Compress("TXT" + clipText + ClipText.TEXT_TYPE_SEP);

        var isBig = new List<object>();
        for (int t = 1; t < 256; t++)
        {
            var dd = new DATA { Type = (PackageType)t };
            isBig.Add(new { t, big = dd.IsBigPackage });
        }

        Console.WriteLine(JsonSerializer.Serialize(new
        {
            hashes,
            derived_key = new { key = Keys[0], salt = Hex(salt), dk = derived },
            stream_key = Keys[0],
            magic = Encryption.MagicNumber,
            packets,
            stream,
            clip = new { text = clipText, data = Hex(clip) },
            is_big = isBig,
        }));
    }

    private static void DecodeStream(string key, string hex)
    {
        Encryption.MyKey = key;
        Encryption.MagicNumber = Encryption.Get24BitHash(key);
        var ms = new MemoryStream(Convert.FromHexString(hex));
        var dec = Encryption.GetDecryptedStream(ms);
        Common.SendOrReceiveARandomDataBlockPerInitialIV(dec, false);
        var list = new List<object>();
        while (true)
        {
            byte[] buf = new byte[64];
            int n = dec.ReadEx(buf, 0, 32);
            if (n != 32)
            {
                break;
            }

            bool valid = Wire.ProcessReceivedDataEx(buf);
            var p = new DATA(buf);
            if (p.IsBigPackage)
            {
                if (dec.ReadEx(buf, 32, 32) != 32)
                {
                    break;
                }

                p.Bytes = buf;
            }

            list.Add(Describe(p, valid));
        }

        Console.WriteLine(JsonSerializer.Serialize(list));
    }

    private static object Describe(DATA p, bool valid = true) => new
    {
        type = (int)p.Type,
        valid,
        id = p.Id,
        src = (uint)p.Src,
        des = (uint)p.Des,
        x = p.Md.X,
        y = p.Md.Y,
        wheel = p.Md.WheelDelta,
        flags = p.Md.dwFlags,
        vk = p.Kd.wVk,
        kflags = p.Kd.dwFlags,
        name = p.MachineName,
    };

    // ------------------------------------------------------------------ live peer

    private sealed class TcpSk
    {
        internal Socket BackingSocket;
        private Stream encryptedStream;
        private Stream decryptedStream;
        internal readonly object SendLock = new();

        public Stream EncryptedStream
        {
            get
            {
                if (encryptedStream == null && BackingSocket.Connected)
                {
                    encryptedStream = Encryption.GetEncryptedStream(new NetworkStream(BackingSocket));
                    Common.SendOrReceiveARandomDataBlockPerInitialIV(encryptedStream);
                }

                return encryptedStream;
            }
        }

        public Stream DecryptedStream
        {
            get
            {
                if (decryptedStream == null && BackingSocket.Connected)
                {
                    decryptedStream = Encryption.GetDecryptedStream(new NetworkStream(BackingSocket));
                    Common.SendOrReceiveARandomDataBlockPerInitialIV(decryptedStream, false);
                }

                return decryptedStream;
            }
        }
    }

    private static uint MachineID;
    private static string MachineName;

    private static void TcpSend(TcpSk tcp, DATA data)
    {
        lock (tcp.SendLock)
        {
            if (data.Src == ID.NONE)
            {
                data.Src = (ID)MachineID;
            }

            Wire.TcpSendData(tcp.EncryptedStream, data.Bytes);
        }
    }

    private static void SkSend(TcpSk tcp, DATA data)
    {
        data.Id = Interlocked.Increment(ref Package.PackageID);
        TcpSend(tcp, data);
    }

    private static int RunPeer(string[] a)
    {
        string mode = a[1], host = a[2];
        int basePort = int.Parse(a[3]);
        Encryption.MyKey = a[4];
        Encryption.MagicNumber = Encryption.Get24BitHash(a[4]);
        MachineName = a[5];
        MachineID = uint.Parse(a[6]);
        string script = a[7];
        int seconds = int.Parse(a[8]);
        string clip = a.Length > 9 ? a[9] : "clipboard from C#";
        Package.PackageID = 1000;

        var timer = new Timer(_ => { Emit(new { @event = "timeout" }); Environment.Exit(0); }, null, seconds * 1000, Timeout.Infinite);

        Socket s;
        if (mode == "serve")
        {
            var server = new TcpListener(IPAddress.Loopback, basePort + 1);
            server.Start();
            Emit(new { @event = "listening", port = basePort + 1 });
            s = server.AcceptSocket();
        }
        else
        {
            var client = new TcpClient();
            client.Connect(host, basePort + 1);
            s = client.Client;
        }

        Emit(new { @event = "connected" });
        var tcp = new TcpSk { BackingSocket = s };
        MainTcpRoutine(tcp, script, clip);
        Emit(new { @event = "closed" });
        return 0;
    }

    // Port of SocketStuff.MainTCPRoutine (minus UI / machine pool bookkeeping).
    private static void MainTcpRoutine(TcpSk tcp, string script, string clip)
    {
        int packageCount = 0;
        ID remoteID = ID.NONE;
        byte[] buf = RandomNumberGenerator.GetBytes(Package.PACKAGE_SIZE_EX);
        DATA d = new DATA(buf);
        tcp.BackingSocket.NoDelay = true;
        d.MachineName = MachineName;
        d.Type = PackageType.Handshake;
        for (int i = 0; i < 10; i++)
        {
            TcpSend(tcp, d);
        }

        d.Machine1 = ~d.Machine1;
        d.Machine2 = ~d.Machine2;
        d.Machine3 = ~d.Machine3;
        d.Machine4 = ~d.Machine4;

        var clipBuf = new MemoryStream();
        while (true)
        {
            DATA package;
            try
            {
                package = Wire.TcpReceiveData(tcp.DecryptedStream, out int receivedCount);
            }
            catch (Exception e)
            {
                Emit(new { @event = "error", message = e.Message });
                return;
            }

            remoteID = package.Src;
            if (package.Type == PackageType.Error)
            {
                Emit(new { @event = "eof" });
                return;
            }

            if (package.Type == PackageType.Handshake)
            {
                package.Type = PackageType.HandshakeAck;
                package.Src = ID.NONE;
                package.MachineName = MachineName;
                package.Machine1 = ~package.Machine1;
                package.Machine2 = ~package.Machine2;
                package.Machine3 = ~package.Machine3;
                package.Machine4 = ~package.Machine4;
                TcpSend(tcp, package);
            }
            else if (packageCount >= 0)
            {
                if (++packageCount >= 10)
                {
                    Emit(new { @event = "invalid_key" });
                    return;
                }
                else if (package.Type == PackageType.HandshakeAck)
                {
                    if (package.Machine1 == d.Machine1 && package.Machine2 == d.Machine2 &&
                       package.Machine3 == d.Machine3 && package.Machine4 == d.Machine4)
                    {
                        packageCount = -1;
                        uint rid = (uint)remoteID;
                        Emit(new { @event = "trusted", remote_name = package.MachineName, remote_id = rid });

                        // Common.SendHeartBeat(initial: true)
                        var hb = new DATA { Type = PackageType.Heartbeat, Des = ID.ALL };
                        hb.MachineName = MachineName;
                        SkSend(tcp, hb);

                        if (script == "control")
                        {
                            new Thread(() => Control(tcp, (ID)rid, clip)) { IsBackground = true }.Start();
                        }
                    }
                    else
                    {
                        Emit(new { @event = "invalid_ack" });
                        return;
                    }
                }
            }
            else if (package.Type == PackageType.HandshakeAck)
            {
                // Skipping the rest of the Handshake packages.
            }
            else if (package.Type is PackageType.ClipboardText)
            {
                clipBuf.Write(package.Bytes, 64 - ClipText.DATA_SIZE, ClipText.DATA_SIZE);
            }
            else if (package.Type is PackageType.ClipboardDataEnd)
            {
                string text = ClipText.Decompress(clipBuf.GetBuffer());
                clipBuf = new MemoryStream();
                Emit(new { @event = "clipboard", text });
            }
            else
            {
                Emit(new { @event = "packet", p = Describe(package) });
            }
        }
    }

    private static void Mouse(TcpSk tcp, ID des, int x, int y, int flags, int wheel = 0)
    {
        var m = new DATA { Type = PackageType.Mouse, Des = des };
        m.Md.X = x; m.Md.Y = y; m.Md.dwFlags = flags; m.Md.WheelDelta = wheel;
        SkSend(tcp, m);
        Thread.Sleep(30);
    }

    private static void Key(TcpSk tcp, ID des, int vk, int flags)
    {
        var k = new DATA { Type = PackageType.Keyboard, Des = des };
        k.Kd.wVk = vk; k.Kd.dwFlags = flags;
        k.DateTime = DateTime.Now.Ticks / 10000;
        SkSend(tcp, k);
        Thread.Sleep(30);
    }

    // Behaves like a C# controller that has switched to the Rust machine.
    private static void Control(TcpSk tcp, ID des, string clip)
    {
        Thread.Sleep(300);
        var ms = new DATA { Type = PackageType.MachineSwitched, Des = des };
        ms.MachineName = MachineName;
        SkSend(tcp, ms);
        Mouse(tcp, des, 32768, 16384, 0x200);
        Mouse(tcp, des, 32768, 16384, 0x201);
        Mouse(tcp, des, 32768, 16384, 0x202);
        Mouse(tcp, des, 32768, 16384, 0x20A, -120);
        Mouse(tcp, des, 100000 + 10, -(100000 + 5), 0x200); // relative +10, -5
        Key(tcp, des, 0x41, 0);
        Key(tcp, des, 0x41, 0x80);
        Key(tcp, des, 0x25, 0x01); // VK_LEFT, extended
        Key(tcp, des, 0x25, 0x81);

        // Clipboard.SendClipboardDataUsingTCP
        byte[] bytes = ClipText.Compress("TXT" + clip + ClipText.TEXT_TYPE_SEP);
        int l = bytes.Length, index = 0, len;
        DATA package = new();
        byte[] b = new byte[Package.PACKAGE_SIZE_EX];
        int dataStart = Package.PACKAGE_SIZE_EX - ClipText.DATA_SIZE;
        while (true)
        {
            if ((index + ClipText.DATA_SIZE) > l)
            {
                len = l - index;
                Array.Clear(b, 0, Package.PACKAGE_SIZE_EX);
            }
            else
            {
                len = ClipText.DATA_SIZE;
            }

            Array.Copy(bytes, index, b, dataStart, len);
            package.Bytes = b;
            package.Type = PackageType.ClipboardText;
            package.Des = ID.ALL;
            SkSend(tcp, package);
            index += ClipText.DATA_SIZE;
            if (index >= l)
            {
                break;
            }
        }

        package.Type = PackageType.ClipboardDataEnd;
        package.Des = ID.ALL;
        SkSend(tcp, package);
        Thread.Sleep(100);

        var hide = new DATA { Type = PackageType.HideMouse, Des = des };
        hide.MachineName = MachineName;
        SkSend(tcp, hide);
        Emit(new { @event = "control_done" });
    }
}
