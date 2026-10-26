// Protocol code copied from PowerToys src/modules/MouseWithoutBorders/App
// (Core/DATA.cs, PackageType.cs, ID.cs, MOUSEDATA.cs, KEYBDDATA.cs, Package.cs,
// Encryption.cs, Common.cs, Class/SocketStuff.cs, Class/Extensions.cs, Core/Clipboard.cs).
// Logging / UI / settings removed; logic kept verbatim. MIT licensed (Microsoft).
using System;
using System.IO;
using System.IO.Compression;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;

namespace MouseWithoutBorders.Core;

internal enum PackageType // : int
{
    Invalid = 0xFF,
    Error = 0xFE,
    Hi = 2,
    Hello = 3,
    ByeBye = 4,
    Heartbeat = 20,
    Awake = 21,
    HideMouse = 50,
    Heartbeat_ex = 51,
    Heartbeat_ex_l2 = 52,
    Heartbeat_ex_l3 = 53,
    Clipboard = 69,
    ClipboardDragDrop = 70,
    ClipboardDragDropEnd = 71,
    ExplorerDragDrop = 72,
    ClipboardCapture = 73,
    CaptureScreenCommand = 74,
    ClipboardDragDropOperation = 75,
    ClipboardDataEnd = 76,
    MachineSwitched = 77,
    ClipboardAsk = 78,
    ClipboardPush = 79,
    NextMachine = 121,
    Keyboard = 122,
    Mouse = 123,
    ClipboardText = 124,
    ClipboardImage = 125,
    Handshake = 126,
    HandshakeAck = 127,
    Matrix = 128,
    MatrixSwapFlag = 2,
    MatrixTwoRowFlag = 4,
}

internal enum ID : uint
{
    NONE = 0,
    ALL = 255,
}

internal enum ClipboardPostAction : uint
{
    Other = 0,
    Desktop = 1,
    Mspaint = 2,
}

[StructLayout(LayoutKind.Sequential)]
internal struct MOUSEDATA
{
    internal int X;
    internal int Y;
    internal int WheelDelta;
    internal int dwFlags;
}

[StructLayout(LayoutKind.Sequential)]
internal struct KEYBDDATA
{
    internal int wVk;
    internal int dwFlags;
}

internal static class Package
{
    internal const byte PACKAGE_SIZE = 32;
    internal const byte PACKAGE_SIZE_EX = 64;
    internal static int PackageID;
}

internal static class Common
{
    internal static byte[] GetBytes(string st) => ASCIIEncoding.ASCII.GetBytes(st);

    internal static string GetString(byte[] bytes) => ASCIIEncoding.ASCII.GetString(bytes);

    internal static byte[] GetBytesU(string st) => ASCIIEncoding.Unicode.GetBytes(st);

    internal static string GetStringU(byte[] bytes) => ASCIIEncoding.Unicode.GetString(bytes);

    internal static void SendOrReceiveARandomDataBlockPerInitialIV(Stream st, bool send = true, byte[] fixedBlock = null)
    {
        byte[] ranData = new byte[Encryption.SymAlBlockSize];
        if (send)
        {
            ranData = fixedBlock ?? RandomNumberGenerator.GetBytes(Encryption.SymAlBlockSize);
            st.Write(ranData, 0, ranData.Length);
        }
        else
        {
            int toRead = ranData.Length;
            _ = st.ReadEx(ranData, 0, toRead);
        }
    }
}

internal static class Extensions
{
    internal static int ReadEx(this Stream st, byte[] buf, int bufIndex, int length)
    {
        int bytesReceived = st.Read(buf, bufIndex, length);
        int receivedCount = bytesReceived;
        while (receivedCount != 0 && bytesReceived < length)
        {
            bytesReceived += receivedCount = st.Read(buf, bufIndex + bytesReceived, length - bytesReceived);
        }

        return bytesReceived;
    }
}

[StructLayout(LayoutKind.Explicit)]
internal sealed class DATA
{
    [FieldOffset(0)]
    internal PackageType Type;
    [FieldOffset(sizeof(PackageType))]
    internal int Id;
    [FieldOffset(sizeof(PackageType) + sizeof(uint))]
    internal ID Src;
    [FieldOffset(sizeof(PackageType) + (2 * sizeof(uint)))]
    internal ID Des;
    [FieldOffset(sizeof(PackageType) + (3 * sizeof(uint)))]
    internal long DateTime;
    [FieldOffset(sizeof(PackageType) + (3 * sizeof(uint)) + sizeof(long))]
    internal KEYBDDATA Kd;
    [FieldOffset(sizeof(PackageType) + (3 * sizeof(uint)))]
    internal MOUSEDATA Md;
    [FieldOffset(sizeof(PackageType) + (3 * sizeof(uint)))]
    internal ID Machine1;
    [FieldOffset(sizeof(PackageType) + (4 * sizeof(uint)))]
    internal ID Machine2;
    [FieldOffset(sizeof(PackageType) + (5 * sizeof(uint)))]
    internal ID Machine3;
    [FieldOffset(sizeof(PackageType) + (6 * sizeof(uint)))]
    internal ID Machine4;
    [FieldOffset(sizeof(PackageType) + (3 * sizeof(uint)))]
    internal ClipboardPostAction PostAction;
    [FieldOffset(sizeof(PackageType) + (7 * sizeof(uint)))]
    private long machineNameP1;
    [FieldOffset(sizeof(PackageType) + (7 * sizeof(uint)) + sizeof(long))]
    private long machineNameP2;
    [FieldOffset(sizeof(PackageType) + (7 * sizeof(uint)) + (2 * sizeof(long)))]
    private long machineNameP3;
    [FieldOffset(sizeof(PackageType) + (7 * sizeof(uint)) + (3 * sizeof(long)))]
    private long machineNameP4;

    internal string MachineName
    {
        get
        {
            string name = Common.GetString(BitConverter.GetBytes(machineNameP1))
                + Common.GetString(BitConverter.GetBytes(machineNameP2))
                + Common.GetString(BitConverter.GetBytes(machineNameP3))
                + Common.GetString(BitConverter.GetBytes(machineNameP4));
            return name.Trim();
        }

        set
        {
            byte[] machineName = Common.GetBytes(value.PadRight(32, ' '));
            machineNameP1 = BitConverter.ToInt64(machineName, 0);
            machineNameP2 = BitConverter.ToInt64(machineName, 8);
            machineNameP3 = BitConverter.ToInt64(machineName, 16);
            machineNameP4 = BitConverter.ToInt64(machineName, 24);
        }
    }

    public DATA()
    {
    }

    public DATA(byte[] initialData)
    {
        Bytes = initialData;
    }

    internal byte[] Bytes
    {
        get
        {
            byte[] buf = new byte[IsBigPackage ? Package.PACKAGE_SIZE_EX : Package.PACKAGE_SIZE];
            Array.Copy(StructToBytes(this), buf, IsBigPackage ? Package.PACKAGE_SIZE_EX : Package.PACKAGE_SIZE);
            return buf;
        }

        set
        {
            byte[] buf = new byte[Package.PACKAGE_SIZE_EX];
            Array.Copy(value, buf, value.Length);
            BytesToStruct(buf, this);
        }
    }

    internal bool IsBigPackage
    {
        get => Type == 0
                ? throw new InvalidOperationException("Package type not set.")
                : Type switch
                {
                    PackageType.Hello or PackageType.Awake or PackageType.Heartbeat or PackageType.Heartbeat_ex or PackageType.Handshake or PackageType.HandshakeAck or PackageType.ClipboardPush or PackageType.Clipboard or PackageType.ClipboardAsk or PackageType.ClipboardImage or PackageType.ClipboardText or PackageType.ClipboardDataEnd => true,
                    _ => (Type & PackageType.Matrix) == PackageType.Matrix,
                };
    }

    private byte[] StructToBytes(object structObject)
    {
        byte[] bytes = new byte[Package.PACKAGE_SIZE_EX];
        GCHandle bHandle = GCHandle.Alloc(bytes, GCHandleType.Pinned);
        Marshal.StructureToPtr(structObject, Marshal.UnsafeAddrOfPinnedArrayElement(bytes, 0), false);
        bHandle.Free();
        return bytes;
    }

    private void BytesToStruct(byte[] value, object structObject)
    {
        GCHandle bHandle = GCHandle.Alloc(value, GCHandleType.Pinned);
        Marshal.PtrToStructure(Marshal.UnsafeAddrOfPinnedArrayElement(value, 0), structObject);
        bHandle.Free();
    }
}

internal static class Encryption
{
    private static AesCryptoServiceProvider symAl;
    internal static string MyKey;
    internal static uint MagicNumber;
    internal const int SymAlBlockSize = 16;
    private const int SaltSize = 16;
    private const int KeyDerivationIterations = 100000;
    private const int DerivedKeyLength = 32;

    internal static void InitEncryption()
    {
        if (symAl == null)
        {
            symAl = new AesCryptoServiceProvider();
            symAl.KeySize = 256;
            symAl.BlockSize = SymAlBlockSize * 8;
            symAl.Padding = PaddingMode.Zeros;
            symAl.Mode = CipherMode.CBC;
            symAl.GenerateIV();
        }
    }

    internal static byte[] GenLegalKey(byte[] salt)
    {
        return Rfc2898DeriveBytes.Pbkdf2(Encryption.MyKey, salt, KeyDerivationIterations, HashAlgorithmName.SHA512, DerivedKeyLength);
    }

    // `fixedHeader` added for deterministic test vectors; otherwise identical.
    internal static Stream GetEncryptedStream(Stream encryptedStream, byte[] fixedHeader = null)
    {
        byte[] header = new byte[SaltSize + SymAlBlockSize];
        if (fixedHeader != null)
        {
            Array.Copy(fixedHeader, header, header.Length);
        }
        else
        {
            RandomNumberGenerator.Fill(header);
        }

        encryptedStream.Write(header, 0, header.Length);
        byte[] salt = header[..SaltSize];
        byte[] iv = header[SaltSize..];
        ICryptoTransform encryptor = symAl.CreateEncryptor(GenLegalKey(salt), iv);
        return new CryptoStream(encryptedStream, encryptor, CryptoStreamMode.Write);
    }

    internal static Stream GetDecryptedStream(Stream encryptedStream)
    {
        byte[] header = new byte[SaltSize + SymAlBlockSize];
        encryptedStream.ReadExactly(header);
        byte[] salt = header[..SaltSize];
        byte[] iv = header[SaltSize..];
        ICryptoTransform decryptor = symAl.CreateDecryptor(GenLegalKey(salt), iv);
        return new CryptoStream(encryptedStream, decryptor, CryptoStreamMode.Read);
    }

    internal static uint Get24BitHash(string st)
    {
        if (string.IsNullOrEmpty(st))
        {
            return 0;
        }

        byte[] bytes = new byte[Package.PACKAGE_SIZE];
        for (int i = 0; i < Package.PACKAGE_SIZE; i++)
        {
            if (i < st.Length)
            {
                bytes[i] = (byte)st[i];
            }
        }

        var hash = SHA512.Create();
        byte[] hashValue = hash.ComputeHash(bytes);
        for (int i = 0; i < 50000; i++)
        {
            hashValue = hash.ComputeHash(hashValue);
        }

        hash.Clear();
        return (uint)((hashValue[0] << 23) + (hashValue[1] << 16) + (hashValue[^1] << 8) + hashValue[2]);
    }
}

// SocketStuff.TcpSendData / ProcessReceivedDataEx / TcpReceiveData, on plain streams.
internal static class Wire
{
    internal static void StampForSend(byte[] bytes)
    {
        bytes[3] = (byte)((Encryption.MagicNumber >> 24) & 0xFF);
        bytes[2] = (byte)((Encryption.MagicNumber >> 16) & 0xFF);
        bytes[1] = 0;
        for (int i = 2; i < Package.PACKAGE_SIZE; i++)
        {
            bytes[1] = (byte)(bytes[1] + bytes[i]);
        }
    }

    internal static void TcpSendData(Stream encryptedStream, byte[] bytes)
    {
        StampForSend(bytes);
        encryptedStream.Write(bytes, 0, bytes.Length);
    }

    // Returns false when invalid (also marks buf[0] = Invalid), exactly like the original.
    internal static bool ProcessReceivedDataEx(byte[] buf)
    {
        bool ok = true;
        int magic;
        byte checksum = 0;
        magic = (buf[3] << 24) + (buf[2] << 16);
        if (magic != (Encryption.MagicNumber & 0xFFFF0000))
        {
            buf[0] = (byte)PackageType.Invalid;
            ok = false;
        }

        for (int i = 2; i < Package.PACKAGE_SIZE; i++)
        {
            checksum = (byte)(checksum + buf[i]);
        }

        if (buf[1] != checksum)
        {
            buf[0] = (byte)PackageType.Invalid;
            ok = false;
        }

        buf[3] = buf[2] = buf[1] = 0;
        return ok;
    }

    internal static DATA TcpReceiveData(Stream decryptedStream, out int bytesReceived)
    {
        byte[] buf = new byte[Package.PACKAGE_SIZE_EX];
        DATA package;
        bytesReceived = decryptedStream.ReadEx(buf, 0, Package.PACKAGE_SIZE);
        if (bytesReceived != Package.PACKAGE_SIZE)
        {
            buf[0] = bytesReceived == 0 ? (byte)PackageType.Error : (byte)PackageType.Invalid;
        }
        else
        {
            ProcessReceivedDataEx(buf);
        }

        package = new DATA(buf);
        if (package.IsBigPackage)
        {
            bytesReceived = decryptedStream.ReadEx(buf, Package.PACKAGE_SIZE, Package.PACKAGE_SIZE);
            if (bytesReceived != Package.PACKAGE_SIZE)
            {
                buf[0] = bytesReceived == 0 ? (byte)PackageType.Error : (byte)PackageType.Invalid;
            }
            else
            {
                package.Bytes = buf;
            }
        }

        return package;
    }
}

// Core/Clipboard.cs text path.
internal static class ClipText
{
    internal const string TEXT_TYPE_SEP = "{4CFF57F7-BEDD-43d5-AE8F-27A61E886F2F}";
    internal const int DATA_SIZE = 48;

    internal static byte[] Compress(string stringData)
    {
        byte[] texts = Common.GetBytesU(stringData);
        using MemoryStream ms = new();
        using (DeflateStream s = new(ms, CompressionMode.Compress, true))
        {
            s.Write(texts, 0, texts.Length);
        }

        return ms.GetBuffer(); // includes trailing capacity, like the original
    }

    internal static string Decompress(byte[] data)
    {
        string st = string.Empty;
        using (MemoryStream ms = new(data))
        {
            using DeflateStream s = new(ms, CompressionMode.Decompress, true);
            const int BufferSize = 1024000;
            int rv = 0;
            do
            {
                byte[] buffer = new byte[BufferSize];
                rv = s.ReadEx(buffer, 0, BufferSize);
                if (rv > 0)
                {
                    st += Common.GetStringU(buffer);
                }
                else
                {
                    break;
                }
            }
            while (true);
        }

        string[] texts = st.Split(new string[] { TEXT_TYPE_SEP }, StringSplitOptions.RemoveEmptyEntries);
        foreach (string txt in texts)
        {
            if (string.IsNullOrEmpty(txt.Trim('\0')))
            {
                continue;
            }

            if (txt.StartsWith("TXT", StringComparison.CurrentCultureIgnoreCase))
            {
                return txt[3..];
            }
        }

        return null;
    }
}
