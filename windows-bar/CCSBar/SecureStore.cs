using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace CCSBar;

/// <summary>Current-user Windows DPAPI. Provider credentials never enter this client.</summary>
public static class SecureStore
{
    // Intentional compatibility: branding never rewrites the existing private store or DPAPI purpose.
    // AAC_TRAY_STATE_DIR (an absolute path) isolates test runs, such as the single-instance check, from the real store.
    public static readonly string StateDirectory = Environment.GetEnvironmentVariable("AAC_TRAY_STATE_DIR") is { Length: > 0 } isolated && Path.IsPathFullyQualified(isolated)
        ? isolated : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "CCS Bar");
    public static readonly string SettingsPath = Path.Combine(StateDirectory, "connection.dpapi");
    private static readonly byte[] Entropy = Encoding.UTF8.GetBytes("CCSBar/dashboard-connection/v1");

    [StructLayout(LayoutKind.Sequential)] private struct DataBlob { public int Length; public IntPtr Data; }
    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)] private static extern bool CryptProtectData(ref DataBlob input, string? description, ref DataBlob entropy, IntPtr reserved, IntPtr prompt, int flags, out DataBlob output);
    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)] private static extern bool CryptUnprotectData(ref DataBlob input, IntPtr description, ref DataBlob entropy, IntPtr reserved, IntPtr prompt, int flags, out DataBlob output);
    [DllImport("kernel32.dll")] private static extern IntPtr LocalFree(IntPtr data);

    public static ConnectionSettings? Load()
    {
        if (!File.Exists(SettingsPath)) return null;
        byte[] decrypted = Transform(File.ReadAllBytes(SettingsPath), false);
        try
        {
            var settings = JsonSerializer.Deserialize<ConnectionSettings>(decrypted, Formatting.Json);
            settings?.Validate();
            return settings;
        }
        finally { CryptographicOperations.ZeroMemory(decrypted); }
    }

    public static void Save(ConnectionSettings settings)
    {
        settings.Validate();
        var bytes = JsonSerializer.SerializeToUtf8Bytes(settings, Formatting.Json);
        try
        {
            var encrypted = Transform(bytes, true);
            Directory.CreateDirectory(StateDirectory);
            var temporary = SettingsPath + ".tmp-" + Guid.NewGuid().ToString("N");
            try
            {
                File.WriteAllBytes(temporary, encrypted);
                File.Move(temporary, SettingsPath, true);
            }
            finally { if (File.Exists(temporary)) File.Delete(temporary); }
        }
        finally { CryptographicOperations.ZeroMemory(bytes); }
    }

    internal static bool CheckRoundTrip(byte[] plaintext)
    {
        var ciphertext = Transform(plaintext, true);
        var decoded = Transform(ciphertext, false);
        try { return !Convert.ToBase64String(ciphertext).Contains(Encoding.UTF8.GetString(plaintext), StringComparison.Ordinal) && CryptographicOperations.FixedTimeEquals(plaintext, decoded); }
        finally { CryptographicOperations.ZeroMemory(decoded); }
    }

    private static byte[] Transform(byte[] bytes, bool protect)
    {
        var input = MakeBlob(bytes);
        var entropy = MakeBlob(Entropy);
        var output = new DataBlob();
        try
        {
            // CRYPTPROTECT_UI_FORBIDDEN. No prompts and no machine-wide encryption scope.
            var ok = protect
                ? CryptProtectData(ref input, null, ref entropy, IntPtr.Zero, IntPtr.Zero, 1, out output)
                : CryptUnprotectData(ref input, IntPtr.Zero, ref entropy, IntPtr.Zero, IntPtr.Zero, 1, out output);
            if (!ok) throw new Win32Exception(Marshal.GetLastWin32Error(), "Windows could not unlock the dashboard connection.");
            var result = new byte[output.Length];
            Marshal.Copy(output.Data, result, 0, result.Length);
            return result;
        }
        finally
        {
            ClearBlob(input); ClearBlob(entropy);
            if (output.Data != IntPtr.Zero)
            {
                for (int i = 0; i < output.Length; i++) Marshal.WriteByte(output.Data, i, 0);
                LocalFree(output.Data);
            }
        }
    }

    private static DataBlob MakeBlob(byte[] bytes)
    {
        var data = Marshal.AllocHGlobal(bytes.Length);
        Marshal.Copy(bytes, 0, data, bytes.Length);
        return new DataBlob { Length = bytes.Length, Data = data };
    }

    private static void ClearBlob(DataBlob blob)
    {
        if (blob.Data == IntPtr.Zero) return;
        for (int i = 0; i < blob.Length; i++) Marshal.WriteByte(blob.Data, i, 0);
        Marshal.FreeHGlobal(blob.Data);
    }
}
