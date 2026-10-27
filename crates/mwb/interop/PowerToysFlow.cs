// A PowerToys Mouse Without Borders "PC" driven like the Settings UI's
// Connect button, for interop tests of the pairing path.
//
//   pt <basePort> <machineName> <machineId> <seconds> [--dns NAME=IP,...] [--listen <port>]
//
// then commands on stdin, one per line:
//   name2ip <lines separated by '|'>   Setting.Values.Name2IP (the "IP address mapping" box)
//   connect <pcName> <key>             MouseWithoutBordersViewModel.SubmitConnectionRequestAsync
//                                      -> SettingsSyncHelper.ConnectToMachine (Class/Program.cs)
//   switch <name>                      MachineStuff.SwitchToMachine + some input to it
//   matrix <a,b,c,d>                   the layout editor: MachineMatrix = ...; SendMachineMatrix()
//   clip <text>                        Clipboard text to everyone (SendClipboardDataUsingTCP)
//   status                             sockets / pool / matrix as JSON
//
// Ported (logic kept, UI / logging / telemetry removed) from PowerToys
// src/modules/MouseWithoutBorders/App:
//   Class/Program.cs        SettingsSyncHelper.ConnectToMachine
//   Class/SocketStuff.cs    UpdateTCPClients, StartNewTcpClient (Name2IP + Dns.GetHostEntry + ReverseLookup
//                           + CheckForSameSubNet), StartNewTcpClientThread, MainTCPRoutine, UpdateTcpSockets,
//                           GetName2IpMappingLines, GetMachineNameFromSocket, AddSocket
//   Class/MachinePool.cs    LearnMachine, TryFindMachineByName, TryUpdateMachineID, Initialize, ResolveID
//   Core/MachineStuff.cs    AddToMachinePool, SendMachineMatrix, UpdateMachineMatrix, InMachineMatrix
//   Core/Common.cs          SkSend, IsConnectedByAClientSocketTo, IsConnectingByAClientSocketTo, ReopenSockets
//   Core/Receiver.cs        the Heartbeat / Hello / Awake / Matrix cases
//
// Deviations, all because both "machines" run on one host:
//   * Dns.GetHostEntry is replaced by an in-memory table (--dns) with the semantics of .NET's
//     Dns.GetHostEntry on Windows: an IP-address string does a reverse lookup (getnameinfo with
//     NI_NAMEREQD; throws "No such host is known" without a PTR/name) followed by a forward
//     lookup of the name found; a name not in the table throws. No real DNS traffic.
//   * The listener (--listen) uses its own port instead of TcpPort+1 (the Rust engine has that
//     port on 127.0.0.1); without --listen the PC behaves as if its firewall blocks inbound TCP.
#pragma warning disable CS0162 // the ReverseLookup / SameSubNetOnly branches are kept but off (PowerToys defaults)
using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Security.Cryptography;
using System.Text.Json;
using System.Threading;
using MouseWithoutBorders.Core;

internal static class PtFlow
{
    private const byte MAX_MACHINE = 4;

    private enum SocketStatus
    {
        NA = 0,
        Resolving = 1,
        Connecting = 2,
        Handshaking = 3,
        Error = 4,
        ForceClosed = 5,
        InvalidKey = 6,
        Timeout = 7,
        SendError = 8,
        Connected = 9,
    }

    private sealed class TcpSk
    {
        internal TcpSk(bool isClient, Socket s, SocketStatus status, string machineName, IPAddress address = null)
        {
            IsClient = isClient;
            BackingSocket = s;
            Status = status;
            MachineName = machineName;
            Address = address;
        }

        internal bool IsClient;
        internal Socket BackingSocket;
        internal SocketStatus Status;
        internal string MachineName;
        internal uint MachineId;
        internal IPAddress Address;
        internal int Generation;
        internal readonly object SendLock = new();
        private Stream encryptedStream;
        private Stream decryptedStream;

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

    private struct MachineInf
    {
        internal string Name;
        internal ID Id;
    }

    // ------------------------------------------------------------------ state

    private static string MachineName;          // Common.MachineName
    private static uint MyId;                   // Setting.Values.MachineId
    private static int TcpPort;                 // Setting.Values.TcpPort
    private static string[] MachineMatrix = new string[MAX_MACHINE] { string.Empty, string.Empty, string.Empty, string.Empty };
    private static readonly List<MachineInf> Pool = new();
    private static readonly object PoolLock = new();
    private static string Name2IP = string.Empty;  // Setting.Values.Name2IP
    private const string Name2IpPolicyList = "";    // GPO list (none)
    private const bool ReverseLookup = false;       // ValidateRemoteMachineIP default
    private const bool SameSubNetOnly = false;      // SameSubnetOnly default
    private static Dictionary<string, string> FakeDns = new(StringComparer.OrdinalIgnoreCase);
    private static readonly List<TcpSk> TcpSockets = new();
    private static readonly object TcpSocketsLock = new();
    private static bool InvalidKeyFound;
    private static int Generation;
    private static long ConnectTimeoutAt;
    private static ID DesMachineID;
    private static readonly HashSet<int> SeenIds = new();
    private static readonly object OutLock = new();
    private static readonly string[] Separator = new string[] { "\r\n" };
    private static readonly char[] BlankSeparator = new char[] { ' ' };

    private static void Emit(object o)
    {
        lock (OutLock)
        {
            Console.Out.WriteLine(JsonSerializer.Serialize(o));
            Console.Out.Flush();
        }
    }

    private static long GetTick() => Environment.TickCount64;

    internal static int Run(string[] a)
    {
        TcpPort = int.Parse(a[1], CultureInfo.InvariantCulture);
        MachineName = a[2];
        MyId = uint.Parse(a[3], CultureInfo.InvariantCulture);
        int seconds = int.Parse(a[4], CultureInfo.InvariantCulture);
        int listenPort = 0;
        for (int i = 5; i < a.Length; i++)
        {
            if (a[i] == "--dns" && i + 1 < a.Length)
            {
                foreach (var kv in a[++i].Split(',', StringSplitOptions.RemoveEmptyEntries))
                {
                    var p = kv.Split('=');
                    FakeDns[p[0]] = p[1];
                }
            }
            else if (a[i] == "--listen" && i + 1 < a.Length)
            {
                listenPort = int.Parse(a[++i], CultureInfo.InvariantCulture);
            }
        }

        Package.PackageID = 5000;
        _ = new Timer(_ => { Emit(new { @event = "timeout" }); Environment.Exit(0); }, null, seconds * 1000, Timeout.Infinite);

        // InitAndCleanup: the pool always holds this machine; MachineMatrix = [me].
        ClearComputerMatrix();

        if (listenPort != 0)
        {
            var server = new TcpListener(IPAddress.Loopback, listenPort);
            server.Start();
            new Thread(() => TCPServerThread(server)) { IsBackground = true }.Start();
            Emit(new { @event = "listening", port = listenPort });
        }

        Emit(new { @event = "ready" });
        string line;
        while ((line = Console.In.ReadLine()) != null)
        {
            string[] w = line.Split(' ', 2);
            string arg = w.Length > 1 ? w[1] : string.Empty;
            try
            {
                switch (w[0])
                {
                    case "name2ip":
                        // The Settings UI stores the box's lines joined with "\r\n".
                        Name2IP = arg.Replace("|", "\r\n");
                        Emit(new { @event = "ok", cmd = "name2ip" });
                        break;
                    case "connect":
                        {
                            string[] p = arg.Split(' ', 2);
                            SubmitConnectionRequest(p[0], p[1]);
                            break;
                        }

                    case "switch":
                        SwitchToMachineAndControl(arg.Trim());
                        break;
                    case "matrix":
                        MachineMatrix = arg.Split(',').Select(s => s.Trim()).Concat(Enumerable.Repeat(string.Empty, MAX_MACHINE)).Take(MAX_MACHINE).ToArray();
                        SendMachineMatrix();
                        Emit(new { @event = "ok", cmd = "matrix" });
                        break;
                    case "clip":
                        SendClipboardText(arg);
                        break;
                    case "status":
                        EmitStatus();
                        break;
                    case "quit":
                        return 0;
                }
            }
            catch (Exception e)
            {
                Emit(new { @event = "command_error", cmd = w[0], message = e.Message });
            }
        }

        return 0;
    }

    private static void EmitStatus()
    {
        object sockets, pool;
        lock (TcpSocketsLock)
        {
            sockets = TcpSockets.Select(t => new { name = t.MachineName, id = t.MachineId, client = t.IsClient, status = t.Status.ToString() }).ToArray();
        }

        lock (PoolLock)
        {
            pool = Pool.Select(m => new { name = m.Name, id = (uint)m.Id }).ToArray();
        }

        Emit(new { @event = "status", sockets, pool, matrix = MachineMatrix });
    }

    // ------------------------------------------------------------------ Settings UI -> ConnectToMachine

    // MouseWithoutBordersViewModel.SubmitConnectionRequestAsync: trims, then the IPC call.
    private static void SubmitConnectionRequest(string pcName, string securityKey)
    {
        pcName = pcName?.Trim();
        securityKey = securityKey?.Trim();
        if (string.IsNullOrWhiteSpace(pcName) || string.IsNullOrWhiteSpace(securityKey))
        {
            Emit(new { @event = "pairing_error" });
            return;
        }

        ConnectToMachine(pcName, securityKey);
    }

    // Class/Program.cs SettingsSyncHelper.ConnectToMachine.
    private static void ConnectToMachine(string pcName, string securityKey)
    {
        ClearComputerMatrix();
        Encryption.MyKey = securityKey;
        Encryption.MagicNumber = Encryption.Get24BitHash(Encryption.MyKey);
        MachineMatrix = new string[MAX_MACHINE] { pcName.Trim().ToUpper(CultureInfo.CurrentCulture), MachineName.Trim(), string.Empty, string.Empty };
        PoolInitialize(MachineMatrix);
        Emit(new { @event = "connect_to_machine", matrix = MachineMatrix });

        InvalidKeyFound = false;
        ReopenSockets(true);
        SendMachineMatrix();
    }

    // Core/MachineStuff.cs ClearComputerMatrix.
    private static void ClearComputerMatrix()
    {
        MachineMatrix = new string[MAX_MACHINE] { MachineName.Trim(), string.Empty, string.Empty, string.Empty };
        PoolInitialize(new string[] { MachineName });
        _ = TryUpdateMachineID(MachineName, (ID)MyId, true);
    }

    // Core/Common.cs ReopenSockets: close every socket, new SocketStuff, UpdateClientSockets.
    private static void ReopenSockets(bool byUser)
    {
        Emit(new { @event = "reopen_sockets", by_user = byUser });
        lock (TcpSocketsLock)
        {
            Generation++;
            foreach (var t in TcpSockets)
            {
                try
                {
                    t.BackingSocket?.Close();
                }
                catch (Exception)
                {
                }
            }

            TcpSockets.Clear();
        }

        // SocketStuff ctor: connectTimeout = GetTick() + CONNECT_TIMEOUT / 2
        ConnectTimeoutAt = GetTick() + 30000;
        UpdateClientSockets("ReopenSockets");
    }

    private static void UpdateClientSockets(string logHeader)
    {
        UpdateTCPClients();
    }

    // ------------------------------------------------------------------ SocketStuff (client side)

    private static void UpdateTCPClients()
    {
        if (InvalidKeyFound)
        {
            return;
        }

        foreach (string st in MachineMatrix)
        {
            string machineName = st.Trim();
            if (!string.IsNullOrEmpty(machineName) &&
                !machineName.Equals(MachineName.Trim(), StringComparison.OrdinalIgnoreCase))
            {
                if (IsConnectedByAClientSocketTo(machineName))
                {
                    continue;
                }

                StartNewTcpClient(machineName);
            }
        }
    }

    private static string[] GetName2IpMappingLines(string policyMappings, string userMappings)
    {
        string combinedMappings = policyMappings + Separator[0] + userMappings;
        return combinedMappings.Split(Separator, StringSplitOptions.RemoveEmptyEntries);
    }

    // Stand-in for System.Net.Dns.GetHostEntry(string) (see the header).
    private static IPHostEntry GetHostEntry(string hostNameOrAddress)
    {
        if (IPAddress.TryParse(hostNameOrAddress, out IPAddress address))
        {
            // Dns.GetHostEntryOrAddressesCore(IPAddress): getnameinfo(NI_NAMEREQD), then
            // getaddrinfo(name); throws when there is no name for the address.
            string name = FakeDns.FirstOrDefault(kv => kv.Value == address.ToString()).Key;
            if (name == null)
            {
                throw new SocketException((int)SocketError.HostNotFound);
            }

            return new IPHostEntry { HostName = name, AddressList = new[] { IPAddress.Parse(FakeDns[name]) } };
        }

        if (FakeDns.TryGetValue(hostNameOrAddress, out string ip))
        {
            return new IPHostEntry { HostName = hostNameOrAddress, AddressList = new[] { IPAddress.Parse(ip) } };
        }

        throw new SocketException((int)SocketError.HostNotFound);
    }

    private static IPHostEntry GetHostEntry(IPAddress address) => GetHostEntry(address.ToString());

    private static void StartNewTcpClient(string machineName)
    {
        void ClientThread()
        {
            IPHostEntry host;
            bool useName2IP = false;
            List<IPAddress> validAddresses = new();
            List<IPAddress> validatedAddresses = new();
            string validAddressesSt = string.Empty;

            // (PowerToys uses AddressFamily.Unspecified, which only Windows accepts.)
            Socket dummySocket = new(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
            TcpSk dummyTcp = AddTcpSocket(true, dummySocket, SocketStatus.Resolving, machineName);

            if (!string.IsNullOrEmpty(Name2IP))
            {
                string[] name2ip = GetName2IpMappingLines(Name2IpPolicyList, Name2IP);
                string[] nameNip;

                if (name2ip != null)
                {
                    foreach (string st in name2ip)
                    {
                        nameNip = st.Split(BlankSeparator, StringSplitOptions.RemoveEmptyEntries);

                        if (nameNip != null && nameNip.Length >= 2 && nameNip[0].Trim().Equals(machineName, StringComparison.OrdinalIgnoreCase)
                            && IPAddress.TryParse(nameNip[1].Trim(), out IPAddress ip) && !validAddressesSt.Contains("[" + ip.ToString() + "]"))
                        {
                            validatedAddresses.Add(ip);
                            validAddressesSt += "[" + ip.ToString() + "]";
                        }
                    }
                }

                if (validatedAddresses.Count > 0)
                {
                    useName2IP = true;
                    Emit(new { @event = "name2ip", name = machineName, ips = validatedAddresses.Select(i => i.ToString()).ToArray() });

                    if (!CheckForSameSubNet(validatedAddresses, machineName))
                    {
                        return;
                    }

                    foreach (IPAddress vip in validatedAddresses)
                    {
                        StartNewTcpClientThread(machineName, vip);
                    }

                    validatedAddresses.Clear();
                }
            }

            try
            {
                host = GetHostEntry(machineName);
            }
            catch (SocketException e)
            {
                host = null;
                UpdateTcpSockets(dummyTcp, SocketStatus.Timeout);
                Emit(new { @event = "dns_error", name = machineName, message = e.Message });
            }

            UpdateTcpSockets(dummyTcp, SocketStatus.NA);

            if (!InMachineMatrix(machineName))
            {
                return;
            }

            if (host != null)
            {
                foreach (IPAddress ip in host.AddressList)
                {
                    if ((ip.AddressFamily == AddressFamily.InterNetwork || ip.AddressFamily == AddressFamily.InterNetworkV6) && !validAddressesSt.Contains("[" + ip.ToString() + "]"))
                    {
                        validAddresses.Add(ip);
                        validAddressesSt += "[" + ip.ToString() + "]";
                    }
                }
            }

            if (validAddresses.Count > 0)
            {
                if (!ReverseLookup)
                {
                    validatedAddresses = validAddresses;
                }
                else
                {
                    foreach (IPAddress ip in validAddresses)
                    {
                        try
                        {
                            string hn = GetHostEntry(ip).HostName;
                            if (hn.StartsWith(machineName, StringComparison.CurrentCultureIgnoreCase) || hn.Equals(ip.ToString(), StringComparison.OrdinalIgnoreCase))
                            {
                                validatedAddresses.Add(ip);
                            }
                        }
                        catch (SocketException)
                        {
                        }
                    }
                }
            }

            if (validatedAddresses.Count > 0)
            {
                if (!CheckForSameSubNet(validatedAddresses, machineName))
                {
                    return;
                }

                foreach (IPAddress ip in validatedAddresses)
                {
                    StartNewTcpClientThread(machineName, ip);
                }
            }
            else
            {
                // "Cannot resolve IPv4 Addresses of machine" - and nothing else happens.
                Emit(new { @event = "no_address", name = machineName, name2ip = useName2IP });
            }
        }

        new Thread(ClientThread) { IsBackground = true }.Start();
    }

    private static bool CheckForSameSubNet(List<IPAddress> validatedAddresses, string machineName)
    {
        if (!SameSubNetOnly)
        {
            return true;
        }

        return true; // (only the default, SameSubnetOnly = false, is exercised)
    }

    private static void StartNewTcpClientThread(string machineName, IPAddress ip)
    {
        void NewTcpClient()
        {
            TcpClient tcpClient = null;
            try
            {
                tcpClient = new TcpClient(AddressFamily.InterNetworkV6);
                tcpClient.Client.DualMode = true;

                if (IsConnectedByAClientSocketTo(machineName))
                {
                    return;
                }

                if (IsConnectingByAClientSocketTo(machineName, ip))
                {
                    return;
                }

                TcpSk tcp = AddTcpSocket(true, tcpClient.Client, SocketStatus.Connecting, machineName, ip);
                UpdateTcpSockets(tcp, SocketStatus.Connecting);
                Emit(new { @event = "connecting", name = machineName, ip = ip.ToString(), port = TcpPort + 1 });

                do
                {
                    try
                    {
                        tcpClient.Connect(ip, TcpPort + 1);
                    }
                    catch (ObjectDisposedException)
                    {
                        return;
                    }
                    catch (SocketException e)
                    {
                        if (ConnectTimeoutAt - GetTick() > 0)
                        {
                            Thread.Sleep(1000);
                            continue;
                        }

                        Emit(new { @event = "connect_timeout", name = machineName, ip = ip.ToString(), message = e.Message });
                        UpdateTcpSockets(tcp, SocketStatus.Timeout);
                        return;
                    }

                    break;
                }
                while (true);

                Emit(new { @event = "tcp_connected", name = machineName, ip = ip.ToString() });
                MainTCPRoutine(tcp, machineName, true);
            }
            catch (Exception e)
            {
                Emit(new { @event = "client_error", name = machineName, message = e.Message });
            }
        }

        new Thread(NewTcpClient) { IsBackground = true }.Start();
    }

    // ------------------------------------------------------------------ SocketStuff (server side)

    private static void TCPServerThread(TcpListener server)
    {
        try
        {
            while (true)
            {
                Socket s = server.AcceptSocket();
                new Thread(() => AddSocket(s)) { IsBackground = true }.Start();
            }
        }
        catch (Exception e)
        {
            Emit(new { @event = "server_error", message = e.Message });
        }
    }

    private static string GetMachineNameFromSocket(Socket socket)
    {
        string stringIP = socket.RemoteEndPoint.ToString();
        string name;
        try
        {
            name = GetHostEntry((socket.RemoteEndPoint as IPEndPoint).Address).HostName;
        }
        catch (SocketException)
        {
            return stringIP;
        }

        if (!string.IsNullOrEmpty(name))
        {
            int dotPos = name.IndexOf('.');
            if (dotPos > 0)
            {
                name = name[..dotPos];
            }
        }

        return string.IsNullOrEmpty(name) ? stringIP : name;
    }

    private static void AddSocket(Socket s)
    {
        string machineName = GetMachineNameFromSocket(s);
        Emit(new { @event = "accepted", name = machineName });
        TcpSk tcp = AddTcpSocket(false, s, SocketStatus.Connecting, machineName);
        MainTCPRoutine(tcp, machineName, false);
    }

    // ------------------------------------------------------------------ MainTCPRoutine

    private static TcpSk AddTcpSocket(bool isClient, Socket s, SocketStatus status, string machineName, IPAddress ip = null)
    {
        lock (TcpSocketsLock)
        {
            var tcp = new TcpSk(isClient, s, status, machineName, ip) { Generation = Generation };
            TcpSockets.Add(tcp);
            return tcp;
        }
    }

    private static void TcpSend(TcpSk tcp, DATA data)
    {
        lock (tcp.SendLock)
        {
            if (data.Src == ID.NONE)
            {
                data.Src = (ID)MyId;
            }

            Wire.TcpSendData(tcp.EncryptedStream, data.Bytes);
        }
    }

    private static void MainTCPRoutine(TcpSk tcp, string machineName, bool isClient)
    {
        int packageCount = 0;
        string remoteMachine = string.Empty;
        ID remoteID = ID.NONE;
        byte[] buf = RandomNumberGenerator.GetBytes(Package.PACKAGE_SIZE_EX);
        DATA d = new DATA(buf);
        Socket currentSocket = tcp.BackingSocket;
        var clipBuf = new MemoryStream();

        try
        {
            currentSocket.NoDelay = true;
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
            UpdateTcpSockets(tcp, SocketStatus.Handshaking);
            remoteMachine = string.IsNullOrEmpty(machineName) ? GetMachineNameFromSocket(currentSocket) : machineName;
        }
        catch (Exception e)
        {
            UpdateTcpSockets(tcp, SocketStatus.ForceClosed);
            currentSocket.Close();
            Emit(new { @event = "socket_error", name = machineName, message = e.Message });
            return;
        }

        while (true)
        {
            DATA package;
            try
            {
                package = Wire.TcpReceiveData(tcp.DecryptedStream, out int receivedCount);
            }
            catch (Exception e)
            {
                UpdateTcpSockets(tcp, SocketStatus.Error);
                currentSocket.Close();
                Emit(new { @event = "closed", name = remoteMachine, client = isClient, message = e.Message });
                break;
            }

            remoteID = package.Src;
            if (package.Type == PackageType.Error)
            {
                UpdateTcpSockets(tcp, SocketStatus.Error);
                currentSocket.Close();
                Emit(new { @event = "closed", name = remoteMachine, client = isClient, message = "eof" });
                break;
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
                    UpdateTcpSockets(tcp, SocketStatus.InvalidKey);
                    currentSocket.Close();
                    Emit(new { @event = "invalid_key", name = remoteMachine });
                    break;
                }
                else if (package.Type == PackageType.HandshakeAck)
                {
                    if (package.Machine1 == d.Machine1 && package.Machine2 == d.Machine2 &&
                        package.Machine3 == d.Machine3 && package.Machine4 == d.Machine4)
                    {
                        string claimedMachineName = package.MachineName;
                        string matrixName = remoteMachine;
                        if (!remoteMachine.Equals(claimedMachineName, StringComparison.Ordinal))
                        {
                            remoteMachine = claimedMachineName;
                            tcp.MachineName = remoteMachine;
                        }

                        if (isClient && IsConnectedByAClientSocketTo(remoteMachine))
                        {
                            UpdateTcpSockets(tcp, SocketStatus.ForceClosed);
                            currentSocket.Close();
                            Emit(new { @event = "duplicate", name = remoteMachine });
                            return;
                        }

                        if (remoteMachine.Equals(MachineName, StringComparison.OrdinalIgnoreCase))
                        {
                            UpdateTcpSockets(tcp, SocketStatus.NA);
                            currentSocket.Close();
                            return;
                        }

                        packageCount = -1; // Trusted
                        InvalidKeyFound = false;
                        tcp.MachineId = (uint)remoteID;
                        tcp.Status = SocketStatus.Connected;
                        UpdateTcpSockets(tcp, SocketStatus.Connected);

                        bool inPool = TryFindMachineByName(remoteMachine, out MachineInf machineInfo);
                        Emit(new
                        {
                            @event = "trusted",
                            client = isClient,
                            dialed_name = matrixName,
                            claimed_name = claimedMachineName,
                            remote_id = (uint)remoteID,
                            in_pool = inPool,
                            in_matrix = InMachineMatrix(remoteMachine),
                        });

                        // Common.SendHeartBeat(initial: true)
                        SendPackage(ID.ALL, PackageType.Heartbeat);

                        if (inPool)
                        {
                            if (machineInfo.Name.Equals(NameFromID(DesMachineID) ?? string.Empty, StringComparison.OrdinalIgnoreCase))
                            {
                                DesMachineID = remoteID;
                            }

                            _ = TryUpdateMachineID(remoteMachine, remoteID, true);
                        }

                        if (!isClient)
                        {
                            UpdateClientSockets("MainTCPRoutine");
                        }
                    }
                    else
                    {
                        UpdateTcpSockets(tcp, SocketStatus.InvalidKey);
                        currentSocket.Close();
                        Emit(new { @event = "invalid_ack", name = remoteMachine });
                        break;
                    }
                }
            }
            else if (package.Type == PackageType.HandshakeAck)
            {
                // Skipping the rest of the Handshake packages.
            }
            else
            {
                ProcessPackage(package, tcp, clipBuf);
            }
        }
    }

    // UpdateTcpSockets: the parts that matter here (duplicate client sockets, removal, renaming).
    private static void UpdateTcpSockets(TcpSk tcp, SocketStatus status)
    {
        if (status == SocketStatus.InvalidKey)
        {
            InvalidKeyFound = true;
        }

        lock (TcpSocketsLock)
        {
            if (status == SocketStatus.Connected && tcp.IsClient)
            {
                foreach (TcpSk t in TcpSockets.Where(item => item.IsClient && !ReferenceEquals(item, tcp) && item.MachineName.Equals(tcp.MachineName, StringComparison.OrdinalIgnoreCase)).ToList())
                {
                    t.Status = SocketStatus.ForceClosed;
                }
            }

            List<TcpSk> toBeRemoved = new();
            foreach (TcpSk t in TcpSockets)
            {
                if (t.Status is SocketStatus.Error or SocketStatus.ForceClosed or SocketStatus.NA or SocketStatus.Timeout or SocketStatus.SendError)
                {
                    try
                    {
                        if (t.BackingSocket != null)
                        {
                            t.MachineName = "$*NotUsed*$";
                            t.Status = 0;
                            t.BackingSocket.Close();
                        }
                    }
                    catch (Exception)
                    {
                    }

                    toBeRemoved.Add(t);
                }
            }

            tcp.Status = status;
            foreach (TcpSk t in toBeRemoved)
            {
                _ = TcpSockets.Remove(t);
            }
        }
    }

    private static bool IsConnectedByAClientSocketTo(string machineName)
    {
        lock (TcpSocketsLock)
        {
            return TcpSockets.Any(t => t != null && t.IsClient && t.Status == SocketStatus.Connected
                && t.BackingSocket != null && t.MachineName.Equals(machineName, StringComparison.OrdinalIgnoreCase));
        }
    }

    private static bool IsConnectingByAClientSocketTo(string machineName, IPAddress ip)
    {
        lock (TcpSocketsLock)
        {
            return TcpSockets.Any(t => t != null && t.IsClient && t.Status == SocketStatus.Connecting
                && t.BackingSocket != null && t.MachineName.Equals(machineName, StringComparison.OrdinalIgnoreCase)
                && t.Address.ToString().Equals(ip.ToString(), StringComparison.OrdinalIgnoreCase));
        }
    }

    // Core/Common.cs SkSend: broadcasts go only to sockets whose name is in the matrix.
    private static void SkSend(DATA data, uint? exceptDes)
    {
        data.Id = Interlocked.Increment(ref Package.PackageID);
        List<TcpSk> targets;
        lock (TcpSocketsLock)
        {
            targets = TcpSockets.Where(t => t != null && t.BackingSocket != null && t.Status == SocketStatus.Connected
                && (t.MachineId == (uint)data.Des || (data.Des == ID.ALL && t.MachineId != exceptDes && InMachineMatrix(t.MachineName)))).ToList();
        }

        foreach (var t in targets)
        {
            try
            {
                TcpSend(t, data);
            }
            catch (Exception)
            {
                t.BackingSocket = null;
            }
        }
    }

    private static void SendPackage(ID des, PackageType packageType)
    {
        DATA package = new();
        package.Type = packageType;
        package.Des = des;
        package.MachineName = MachineName;
        SkSend(package, null);
    }

    // ------------------------------------------------------------------ Receiver

    private static void ProcessPackage(DATA package, TcpSk tcp, MemoryStream clipBuf)
    {
        // Duplicates arrive once per socket.
        lock (SeenIds)
        {
            if (package.Type is not (PackageType.ClipboardText or PackageType.ClipboardImage) && !SeenIds.Add(package.Id))
            {
                return;
            }
        }

        switch (package.Type)
        {
            case PackageType.Heartbeat:
            case PackageType.Heartbeat_ex:
            case PackageType.Awake:
                _ = AddToMachinePool(package);
                break;
            case PackageType.Hello:
                SendPackage(ID.ALL, PackageType.Heartbeat);
                _ = AddToMachinePool(package);
                break;
            case PackageType.Hi:
                break;
            case PackageType.ClipboardText:
                clipBuf.Write(package.Bytes, 64 - ClipText.DATA_SIZE, ClipText.DATA_SIZE);
                break;
            case PackageType.ClipboardDataEnd:
                Emit(new { @event = "clipboard", text = ClipText.Decompress(clipBuf.GetBuffer()) });
                clipBuf.SetLength(0);
                break;
            default:
                if ((package.Type & PackageType.Matrix) == PackageType.Matrix)
                {
                    UpdateMachineMatrix(package);
                }
                else
                {
                    Emit(new
                    {
                        @event = "packet",
                        p = new { type = (int)package.Type, src = (uint)package.Src, des = (uint)package.Des, x = package.Md.X, y = package.Md.Y, wheel = package.Md.WheelDelta, flags = package.Md.dwFlags, vk = package.Kd.wVk, kflags = package.Kd.dwFlags },
                    });
                }

                break;
        }
    }

    // ------------------------------------------------------------------ MachineStuff

    private static string AddToMachinePool(DATA package)
    {
        string name = package.MachineName;
        if (TryFindMachineByName(name, out MachineInf machineInfo))
        {
            _ = TryUpdateMachineID(machineInfo.Name, package.Src, true);
            return machineInfo.Name;
        }

        if (LearnMachine(name))
        {
            _ = TryUpdateMachineID(name, package.Src, true);
        }
        else
        {
            return "The 5th machine";
        }

        // NOTE(yuyoyuppe): automatically active "bidirectional" control between the machines.
        string[] st = new string[MAX_MACHINE];
        Array.Fill(st, string.Empty);
        var machines = ListAllMachines();
        for (int i = 0; i < machines.Count; ++i)
        {
            if (machines[i].Id != ID.NONE && machines[i].Id != ID.ALL)
            {
                st[i] = machines[i].Name;
            }
        }

        MachineMatrix = st;
        Emit(new { @event = "pool_learned", name, matrix = MachineMatrix });
        ReopenSockets(true);
        SendMachineMatrix();
        UpdateClientSockets("AddToMachinePool");
        return name;
    }

    private static void SendMachineMatrix()
    {
        DATA package = new();
        for (int i = 0; i < MachineMatrix.Length; i++)
        {
            package.MachineName = MachineMatrix[i];
            package.Type = PackageType.Matrix;
            package.Src = (ID)(i + 1);
            package.Des = ID.ALL;
            SkSend(package, null);
        }
    }

    private static void UpdateMachineMatrix(DATA package)
    {
        uint i = (uint)package.Src;
        if (i is > 0 and <= MAX_MACHINE)
        {
            MachineMatrix[i - 1] = package.MachineName;
            if (i == MAX_MACHINE)
            {
                Emit(new { @event = "matrix_received", matrix = MachineMatrix });
                UpdateClientSockets("UpdateMachineMatrix");
            }
        }
    }

    private static bool InMachineMatrix(string name)
    {
        if (MachineMatrix == null || string.IsNullOrWhiteSpace(name))
        {
            return false;
        }

        return MachineMatrix.Any(st => !string.IsNullOrWhiteSpace(st) && st.Trim().Equals(name.Trim(), StringComparison.OrdinalIgnoreCase));
    }

    private static string NameFromID(ID id)
    {
        lock (PoolLock)
        {
            return id == ID.NONE ? null : Pool.Where(m => m.Id == id).Select(m => m.Name).FirstOrDefault();
        }
    }

    // MachineStuff.SwitchToMachine, then input like a user moving into that machine.
    private static void SwitchToMachineAndControl(string name)
    {
        ID id = ResolveID(name);
        if (id == ID.NONE)
        {
            Emit(new { @event = "switch_failed", name });
            return;
        }

        DesMachineID = id;
        var ms = new DATA { Type = PackageType.MachineSwitched, Des = id };
        ms.MachineName = MachineName;
        SkSend(ms, null);
        void Mouse(int x, int y, int flags, int wheel = 0)
        {
            var m = new DATA { Type = PackageType.Mouse, Des = id };
            m.Md.X = x; m.Md.Y = y; m.Md.dwFlags = flags; m.Md.WheelDelta = wheel;
            SkSend(m, null);
            Thread.Sleep(30);
        }

        void Key(int vk, int flags)
        {
            var k = new DATA { Type = PackageType.Keyboard, Des = id };
            k.Kd.wVk = vk; k.Kd.dwFlags = flags;
            SkSend(k, null);
            Thread.Sleep(30);
        }

        Mouse(32768, 16384, 0x200);
        Mouse(32768, 16384, 0x201);
        Mouse(32768, 16384, 0x202);
        Key(0x42, 0);
        Key(0x42, 0x80);
        var hide = new DATA { Type = PackageType.HideMouse, Des = id };
        hide.MachineName = MachineName;
        SkSend(hide, null);
        Emit(new { @event = "switch_done", name, id = (uint)id });
    }

    private static void SendClipboardText(string text)
    {
        byte[] bytes = ClipText.Compress("TXT" + text + ClipText.TEXT_TYPE_SEP);
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
            SkSend(package, null);
            index += ClipText.DATA_SIZE;
            if (index >= l)
            {
                break;
            }
        }

        package.Type = PackageType.ClipboardDataEnd;
        package.Des = ID.ALL;
        SkSend(package, null);
        Emit(new { @event = "ok", cmd = "clip" });
    }

    // ------------------------------------------------------------------ MachinePool

    private static bool NamesAreEqual(string name1, string name2) => string.Equals(name1, name2, StringComparison.OrdinalIgnoreCase);

    private static void PoolInitialize(IEnumerable<string> machineNames)
    {
        lock (PoolLock)
        {
            Pool.Clear();
            foreach (string name in machineNames)
            {
                if (string.IsNullOrEmpty(name.Trim()))
                {
                    continue;
                }

                _ = LearnMachine(name);
            }
        }

        _ = TryUpdateMachineID(MachineName, (ID)MyId, true);
    }

    private static bool LearnMachine(string machineName)
    {
        lock (PoolLock)
        {
            if (Pool.Any(m => NamesAreEqual(m.Name, machineName)))
            {
                return false;
            }

            if (Pool.Count >= MAX_MACHINE)
            {
                int slotFound = Pool.FindIndex(m => !InMachineMatrix(m.Name));
                if (slotFound >= 0)
                {
                    Pool.RemoveAt(slotFound);
                }
                else
                {
                    return false;
                }
            }

            Pool.Add(new MachineInf { Name = machineName });
            return true;
        }
    }

    private static bool TryFindMachineByName(string machineName, out MachineInf result)
    {
        lock (PoolLock)
        {
            foreach (var m in Pool)
            {
                if (NamesAreEqual(m.Name, machineName))
                {
                    result = m;
                    return true;
                }
            }
        }

        result = default;
        return false;
    }

    private static bool TryUpdateMachineID(string machineName, ID id, bool updateTimeStamp)
    {
        bool rv = false;
        lock (PoolLock)
        {
            for (int i = 0; i < Pool.Count; i++)
            {
                if (NamesAreEqual(machineName, Pool[i].Name))
                {
                    Pool[i] = new MachineInf { Name = Pool[i].Name, Id = id };
                    rv = true;
                }
                else if (Pool[i].Id == id)
                {
                    Pool[i] = new MachineInf { Name = Pool[i].Name, Id = ID.NONE };
                }
            }
        }

        return rv;
    }

    private static List<MachineInf> ListAllMachines()
    {
        lock (PoolLock)
        {
            return Pool.ToList();
        }
    }

    private static ID ResolveID(string machineName) => TryFindMachineByName(machineName, out MachineInf inf) ? inf.Id : ID.NONE;
}
