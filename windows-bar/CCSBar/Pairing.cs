using System;
using System.Threading.Tasks;

namespace CCSBar;

/// <summary>Pairing without the sign-in screen: the installer's and unattended setup's --configure-stdin.</summary>
public static class Pairing
{
    /// <summary>
    /// Pairs a { baseURL, username, password } connection at once and stores only the device key (version 2). When the
    /// dashboard cannot pair this computer (no pairing yet, local network trust off, not reachable), it stores version 1
    /// as the tray always did. Returns true when it paired. The password is never written in version 2.
    /// </summary>
    public static async Task<bool> ConfigureAsync(ConnectionSettings settings, string path)
    {
        settings.Validate();
        if (settings.HasPassword && !settings.IsPaired)
        {
            try
            {
                var origin = settings.ValidateAddress();
                var local = origin.Scheme != "http" || await LocalNetwork.ResolvesLocally(origin.Host, TimeSpan.FromSeconds(5)) == true;
                if (local)
                {
                    using var api = new AuthApi(origin, TimeSpan.FromSeconds(10));
                    var check = await api.Check();
                    if (check.SupportsPairing && check.CanPair && check.AccessMode == "login")
                    {
                        var installId = settings.InstallId is { Length: 36 } kept && Guid.TryParse(kept, out _) ? kept : Guid.NewGuid().ToString("D");
                        var version = System.Reflection.Assembly.GetExecutingAssembly().GetName().Version?.ToString(3) ?? "1.0.0";
                        var answer = await api.Pair(settings.Username.Trim(), settings.Password!, Environment.MachineName, installId, version);
                        if (PairedDevice.From(answer) is { } device)
                        {
                            var paired = new ConnectionSettings
                            {
                                Version = 2, BaseURL = origin.GetLeftPart(UriPartial.Authority), Username = settings.Username.Trim(), DeviceId = device.DeviceId,
                                DeviceToken = device.Token, InstallId = installId, PairedAt = device.PairedAt ?? DateTimeOffset.UtcNow.ToString("O"), RotateAfter = device.RotateAfter,
                            };
                            using var keyed = new DashboardClient(paired);
                            var me = await keyed.DeviceMe();
                            paired.RotateAfter = me.RotateAfter ?? paired.RotateAfter;
                            SecureStore.Save(paired, path);
                            return true;
                        }
                    }
                }
            }
            catch (Exception) { /* Pairing is unavailable: keep the password sign-in below. */ }
        }
        SecureStore.Save(settings, path);
        return false;
    }
}
