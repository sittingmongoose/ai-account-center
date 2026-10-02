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
    /// <summary>The stored JSON: members that are not set are left out, so a version 1 file keeps exactly
    /// {baseURL, username, password} and a version 2 file never carries a password.</summary>
    private static readonly JsonSerializerOptions StoreJson = new(Formatting.Json) { DefaultIgnoreCondition = System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull };

    /// <summary>The version 1 rollback copy kept next to the store while a migration's new device key has not yet
    /// worked once (CONTRACT-auth-devices section 8): same DPAPI scope and entropy, because it is the same bytes.</summary>
    public static string RollbackPath(string path) => Path.Combine(Path.GetDirectoryName(Path.GetFullPath(path))!, "connection.v1-rollback.dpapi");

    [StructLayout(LayoutKind.Sequential)] private struct DataBlob { public int Length; public IntPtr Data; }
    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)] private static extern bool CryptProtectData(ref DataBlob input, string? description, ref DataBlob entropy, IntPtr reserved, IntPtr prompt, int flags, out DataBlob output);
    [DllImport("crypt32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)] private static extern bool CryptUnprotectData(ref DataBlob input, IntPtr description, ref DataBlob entropy, IntPtr reserved, IntPtr prompt, int flags, out DataBlob output);
    [DllImport("kernel32.dll")] private static extern IntPtr LocalFree(IntPtr data);

    public static ConnectionSettings? Load() => Load(SettingsPath);

    /// <summary>The same store at another path: the checks' isolated folders. The tray itself uses <see cref="SettingsPath"/>.</summary>
    public static ConnectionSettings? Load(string path)
    {
        if (!File.Exists(path)) return null;
        byte[] decrypted = Transform(File.ReadAllBytes(path), false);
        try
        {
            var settings = JsonSerializer.Deserialize<ConnectionSettings>(decrypted, Formatting.Json);
            settings?.ValidateStored();
            return settings;
        }
        finally { CryptographicOperations.ZeroMemory(decrypted); }
    }

    /// <summary>Migration step 1: copies the version 1 file, byte for byte, to the rollback file. Its 24 hours count from
    /// now: a Windows copy keeps the source's last-write time, so without the stamp a version 1 file written weeks ago
    /// would make the rollback count as expired the moment it is made (review B5N finding 2).</summary>
    public static void KeepRollback(string path)
    {
        var rollback = RollbackPath(path);
        var temporary = rollback + ".tmp-" + Guid.NewGuid().ToString("N");
        try
        {
            File.Copy(path, temporary, true);
            File.SetLastWriteTimeUtc(temporary, DateTime.UtcNow);
            File.Move(temporary, rollback, true);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
    }

    /// <summary>A 401 device code on the first check: the version 1 file goes back exactly as it was.</summary>
    public static void RestoreRollback(string path)
    {
        var rollback = RollbackPath(path);
        if (!File.Exists(rollback)) return;
        File.Move(rollback, path, true);
    }

    public static void DeleteRollback(string path)
    {
        var rollback = RollbackPath(path);
        if (File.Exists(rollback)) File.Delete(rollback);
    }

    /// <summary>The rollback file's age since <see cref="KeepRollback"/> made it, or null when there is none (it never
    /// lives longer than 24 hours).</summary>
    public static TimeSpan? RollbackAge(string path)
    {
        var rollback = RollbackPath(path);
        return File.Exists(rollback) ? DateTime.UtcNow - File.GetLastWriteTimeUtc(rollback) : null;
    }

    /// <summary>The rollback copy, read for the restore decision only (the checks also read it).</summary>
    public static ConnectionSettings? LoadRollback(string path) => File.Exists(RollbackPath(path)) ? Load(RollbackPath(path)) : null;

    public static void Save(ConnectionSettings settings) => Save(settings, SettingsPath);

    /// <summary>Writes the whole file or nothing (a temporary file, then a replacing move), with the same JSON shape,
    /// DPAPI scope and entropy whatever the path.</summary>
    public static void Save(ConnectionSettings settings, string path)
    {
        settings.ValidateStored();
        if (settings.IsPaired && settings.HasPassword) throw new ArgumentException("A paired connection never stores the password.");
        var bytes = JsonSerializer.SerializeToUtf8Bytes(settings, StoreJson);
        try
        {
            var encrypted = Transform(bytes, true);
            Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
            var temporary = path + ".tmp-" + Guid.NewGuid().ToString("N");
            try
            {
                File.WriteAllBytes(temporary, encrypted);
                File.Move(temporary, path, true);
            }
            finally { if (File.Exists(temporary)) File.Delete(temporary); }
        }
        finally { CryptographicOperations.ZeroMemory(bytes); }
    }

    /// <summary>Checks only, on fixture files: the JSON property names a stored file holds, never their values.</summary>
    internal static string[] StoredKeysForCheck(string path)
    {
        byte[] decrypted = Transform(File.ReadAllBytes(path), false);
        try
        {
            using var json = JsonDocument.Parse(decrypted);
            var keys = new System.Collections.Generic.List<string>();
            foreach (var property in json.RootElement.EnumerateObject()) keys.Add(property.Name);
            return keys.ToArray();
        }
        finally { CryptographicOperations.ZeroMemory(decrypted); }
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
