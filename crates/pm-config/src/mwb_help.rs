//! What to do on the Windows PC to pair it with this Mac (PowerToys Mouse
//! Without Borders). Shared by `powermac mwb doctor` and the settings
//! window's "Pairing with Windows" card.

/// The name to type in PowerToys' "Connect" dialog: Windows compares
/// machine names in upper case.
pub fn connect_name(name: &str) -> String {
    name.trim().to_uppercase()
}

/// One line of PowerToys' Advanced → "IP address mapping" (name, space,
/// address), for when the PC can't resolve the Mac's name.
pub fn ip_mapping_line(name: &str, ip: &str) -> String {
    format!("{} {}", connect_name(name), ip.trim())
}

/// The PowerShell checks for the PC, one command per line (comments explain
/// each one). `key_hint` describes where the security key comes from.
pub fn powershell_steps(name: &str, ip: &str, port: u16, key_hint: &str) -> String {
    let upper = connect_name(name);
    let msg = port.wrapping_add(1);
    format!(
        r#"# 1. Can the PC reach this Mac's Mouse Without Borders port?
Test-NetConnection {ip} -Port {msg}
# 2. Does the Mac's name resolve the way PowerToys resolves it (Dns.GetHostEntry)?
Resolve-DnsName {upper} -LlmnrOnly
[System.Net.Dns]::GetHostEntry("{upper}").AddressList
#    No answer: LLMNR may be off by policy (EnableMulticast = 0 here means off):
Get-ItemProperty 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\DNSClient' -Name EnableMulticast -ErrorAction SilentlyContinue
#    ...or the network is "Public" (run as administrator to change it):
Get-NetConnectionProfile
Set-NetConnectionProfile -InterfaceAlias "Wi-Fi" -NetworkCategory Private
# 3. Optional (one direction is enough): let the Mac connect to the PC too (administrator):
New-NetFirewallRule -DisplayName "Mouse Without Borders (PowerMac)" -Direction Inbound -Protocol TCP -LocalPort {base},{msg} -RemoteAddress {ip} -Action Allow -Profile Private,Domain
# 4. If step 2 finds nothing: PowerToys Settings > Mouse Without Borders > Advanced >
#    "IP address mapping", one line (name, space, address):
#        {mapping}
# 5. PowerToys Settings > Mouse Without Borders > "Connect": security key {key_hint},
#    computer name {upper}   (the NAME, not the IP address)"#,
        base = port,
        mapping = ip_mapping_line(name, ip),
    )
}

/// The individual commands of [`powershell_steps`] (without the comments),
/// as (what it checks, command) for copy buttons.
pub fn powershell_commands(name: &str, ip: &str, port: u16) -> Vec<(&'static str, String)> {
    let upper = connect_name(name);
    let msg = port.wrapping_add(1);
    vec![
        ("Can the PC reach this Mac?", format!("Test-NetConnection {ip} -Port {msg}")),
        ("Does the Mac's name resolve (LLMNR)?", format!("Resolve-DnsName {upper} -LlmnrOnly")),
        ("The way PowerToys resolves it", format!("[System.Net.Dns]::GetHostEntry(\"{upper}\").AddressList")),
        ("Is the network Private? (Public blocks name lookups)", "Get-NetConnectionProfile".to_string()),
        (
            "Optional, as administrator: let the Mac connect to the PC too",
            format!(
                "New-NetFirewallRule -DisplayName \"Mouse Without Borders (PowerMac)\" -Direction Inbound -Protocol TCP -LocalPort {port},{msg} -RemoteAddress {ip} -Action Allow -Profile Private,Domain"
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_use_the_name_and_ip() {
        let t = powershell_steps("MacBook-Pro", "10.0.0.112", 15100, "K");
        assert!(t.contains("Test-NetConnection 10.0.0.112 -Port 15101"));
        assert!(t.contains("Resolve-DnsName MACBOOK-PRO -LlmnrOnly"));
        assert!(t.contains("#        MACBOOK-PRO 10.0.0.112\n"));
        assert!(t.contains("-LocalPort 15100,15101 -RemoteAddress 10.0.0.112"));
        assert_eq!(ip_mapping_line(" MacBook-Pro ", "10.0.0.112"), "MACBOOK-PRO 10.0.0.112");
        let c = powershell_commands("mac", "10.0.0.112", 15100);
        assert_eq!(c[0].1, "Test-NetConnection 10.0.0.112 -Port 15101");
        assert!(c.iter().all(|(_, cmd)| !cmd.contains('#')));
    }
}
