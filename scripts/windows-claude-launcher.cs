using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

// Fixed per-user URI launcher. Profile paths/arguments are derived only from the
// allowlist, never from URI data. Store updates cannot pin named profiles to an
// obsolete WindowsApps version. The native catalog reads current-user packages.
public sealed class ClaudeRegisteredPackage
{
    public readonly string FullName;
    public readonly string InstallPath;
    public ClaudeRegisteredPackage(string fullName, string installPath)
    { FullName = fullName; InstallPath = installPath; }
}

public interface IClaudeRegisteredPackageCatalog
{
    IList<ClaudeRegisteredPackage> ReadCurrentUserClaudePackages();
}

public sealed class ClaudeLaunchPlan
{
    public string ProfileId;
    public string Executable;
    public string Arguments;
    public string WorkingDirectory;
    public string ProfilePath;
    public string DefaultAppTarget;
}

public static class ClaudeProfileLaunchPlanner
{
    public const string Family = "Claude_pzs8sxrjxfjjc";
    public const string DefaultTarget = "shell:AppsFolder\\Claude_pzs8sxrjxfjjc!Claude";
    public static string ProfileId(string uri)
    {
        foreach (string id in new[] { "gmail", "platyr", "party", "me" })
            if (uri == "ccs-claude://launch/" + id) return id;
        return null;
    }

    private static Version PackageVersion(string fullName)
    {
        if (String.IsNullOrEmpty(fullName)) return null;
        string[] fields = fullName.Split('_');
        Version version;
        // Exclude resource packages and foreign publishers, even in fixtures.
        if (fields.Length != 5 || fields[0] != "Claude" || fields[3] != "" ||
            fields[4] != "pzs8sxrjxfjjc" ||
            (fields[2] != "x64" && fields[2] != "x86" &&
             fields[2] != "arm64" && fields[2] != "neutral") ||
            !Version.TryParse(fields[1], out version)) return null;
        return version;
    }

    private static string EnvironmentRoot(string value)
    {
        if (String.IsNullOrWhiteSpace(value) || !Path.IsPathRooted(value) ||
            value.IndexOfAny(new[] { '"', '\0', '\r', '\n' }) >= 0)
            throw new InvalidOperationException("Saved Windows environment root unavailable.");
        string root = Path.GetPathRoot(value);
        if (String.IsNullOrEmpty(root) || root == "\\" || root == "/" ||
            (root.Length == 2 && root[1] == ':'))
            throw new InvalidOperationException("Saved Windows environment root unavailable.");
        return Path.GetFullPath(value);
    }

    public static ClaudeLaunchPlan Create(string uri, string windowsDirectory,
        string appDataDirectory, IClaudeRegisteredPackageCatalog catalog,
        Func<string, bool> fileExists, Func<string, bool> directoryExists)
    {
        string id = ProfileId(uri);
        if (id == null) throw new ArgumentException("Unsupported Claude account link.");
        if (id == "gmail")
        {
            string explorer = Path.Combine(EnvironmentRoot(windowsDirectory), "explorer.exe");
            if (!fileExists(explorer)) throw new InvalidOperationException("Default Claude launcher unavailable.");
            return new ClaudeLaunchPlan { ProfileId = id, Executable = explorer,
                Arguments = DefaultTarget, DefaultAppTarget = DefaultTarget };
        }
        string profile = Path.GetFullPath(Path.Combine(EnvironmentRoot(appDataDirectory), "Claude-" + id));
        if (!Path.IsPathRooted(profile) || profile.IndexOf('"') >= 0 ||
            !directoryExists(profile))
            throw new InvalidOperationException("The saved Claude profile is unavailable.");

        IList<ClaudeRegisteredPackage> packages = catalog.ReadCurrentUserClaudePackages();
        if (packages == null || packages.Count > 32)
            throw new InvalidOperationException("Current Claude package unavailable.");
        ClaudeRegisteredPackage latest = null;
        Version latestVersion = null;
        foreach (ClaudeRegisteredPackage package in packages)
        {
            if (package == null) continue;
            Version version = PackageVersion(package.FullName);
            if (version == null || String.IsNullOrEmpty(package.InstallPath) ||
                !Path.IsPathRooted(package.InstallPath)) continue;
            if (latestVersion == null || version > latestVersion)
            { latest = package; latestVersion = version; }
            else if (version == latestVersion &&
                !String.Equals(package.InstallPath, latest.InstallPath, StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("Current Claude package is ambiguous.");
        }
        if (latest == null) throw new InvalidOperationException("Current Claude package unavailable.");
        string executable = Path.GetFullPath(Path.Combine(latest.InstallPath, "app", "Claude.exe"));
        // Never launch an older package or Gmail when the selected current
        // package/profile is unavailable.
        if (!fileExists(executable))
            throw new InvalidOperationException("Current Claude executable unavailable.");
        return new ClaudeLaunchPlan { ProfileId = id, Executable = executable,
            Arguments = "--user-data-dir=\"" + profile + "\"",
            WorkingDirectory = Path.GetDirectoryName(executable), ProfilePath = profile };
    }
}

public sealed class NativeClaudePackageCatalog : IClaudeRegisteredPackageCatalog
{
    private const int InsufficientBuffer = 122;
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int GetPackagesByPackageFamily(string family,
        ref uint count, IntPtr fullNames, ref uint bufferLength, IntPtr buffer);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int GetPackagePathByFullName(string fullName,
        ref uint pathLength, StringBuilder path);

    public IList<ClaudeRegisteredPackage> ReadCurrentUserClaudePackages()
    {
        uint count = 0, length = 0;
        int code = GetPackagesByPackageFamily(ClaudeProfileLaunchPlanner.Family,
            ref count, IntPtr.Zero, ref length, IntPtr.Zero);
        if (code == 0 && count == 0) return new List<ClaudeRegisteredPackage>();
        if (code != InsufficientBuffer || count == 0 || count > 32 ||
            length == 0 || length > 32768)
            throw new InvalidOperationException("Claude package registration unavailable.");
        uint capacity = count;
        IntPtr names = Marshal.AllocHGlobal(checked((int)count * IntPtr.Size));
        IntPtr buffer = Marshal.AllocHGlobal(checked((int)length * 2));
        try
        {
            code = GetPackagesByPackageFamily(ClaudeProfileLaunchPlanner.Family,
                ref count, names, ref length, buffer);
            if (code != 0 || count > capacity)
                throw new InvalidOperationException("Claude package registration changed.");
            List<ClaudeRegisteredPackage> result = new List<ClaudeRegisteredPackage>();
            for (int index = 0; index < count; index++)
            {
                string fullName = Marshal.PtrToStringUni(Marshal.ReadIntPtr(names, index * IntPtr.Size));
                if (String.IsNullOrEmpty(fullName) || fullName.Length > 256)
                    throw new InvalidOperationException("Claude package identity unavailable.");
                uint pathLength = 0;
                code = GetPackagePathByFullName(fullName, ref pathLength, null);
                if (code != InsufficientBuffer || pathLength == 0 || pathLength > 32768)
                    throw new InvalidOperationException("Claude package path unavailable.");
                StringBuilder path = new StringBuilder((int)pathLength);
                if (GetPackagePathByFullName(fullName, ref pathLength, path) != 0)
                    throw new InvalidOperationException("Claude package path changed.");
                result.Add(new ClaudeRegisteredPackage(fullName, path.ToString()));
            }
            return result;
        }
        finally { Marshal.FreeHGlobal(buffer); Marshal.FreeHGlobal(names); }
    }
}

public static class CcsClaudeAccountLauncher
{
    private static string JsonString(string value)
    {
        if (value == null) return "null";
        StringBuilder text = new StringBuilder("\"");
        foreach (char c in value)
        {
            if (c == '"' || c == '\\') text.Append('\\').Append(c);
            else if (c < 32) text.Append("\\u").Append(((int)c).ToString("x4"));
            else text.Append(c);
        }
        return text.Append('"').ToString();
    }
    public static string Describe(ClaudeLaunchPlan plan)
    {
        return "{\"schema\":1,\"profileId\":" + JsonString(plan.ProfileId) +
            ",\"executable\":" + JsonString(plan.Executable) +
            ",\"arguments\":" + JsonString(plan.Arguments) +
            ",\"workingDirectory\":" + JsonString(plan.WorkingDirectory) +
            ",\"profilePath\":" + JsonString(plan.ProfilePath) +
            ",\"defaultAppTarget\":" + JsonString(plan.DefaultAppTarget) + "}";
    }
    public static int Main(string[] args)
    {
        bool dryRun = args.Length == 2 && args[0] == "--dry-run";
        bool describe = args.Length == 2 && args[0] == "--describe";
        if (args.Length != 1 && !dryRun && !describe)
        { Console.Error.WriteLine("Expected one supported Claude account link."); return 2; }
        string uri = args[(dryRun || describe) ? 1 : 0];
        if (ClaudeProfileLaunchPlanner.ProfileId(uri) == null)
        { Console.Error.WriteLine("Unsupported Claude account link."); return 2; }
        ClaudeLaunchPlan plan;
        try
        {
            plan = ClaudeProfileLaunchPlanner.Create(uri,
                Environment.GetFolderPath(Environment.SpecialFolder.Windows),
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                new NativeClaudePackageCatalog(), File.Exists, Directory.Exists);
        }
        catch { Console.Error.WriteLine("The current Claude app or saved profile is unavailable."); return 3; }
        if (describe) { Console.WriteLine(Describe(plan)); return 0; }
        if (dryRun)
        { Console.WriteLine(plan.ProfileId + "\t" + (plan.DefaultAppTarget ?? plan.Executable)); return 0; }
        try
        {
            ProcessStartInfo start = new ProcessStartInfo(plan.Executable, plan.Arguments);
            start.UseShellExecute = plan.ProfileId == "gmail";
            if (plan.WorkingDirectory != null) start.WorkingDirectory = plan.WorkingDirectory;
            Process.Start(start);
            return 0;
        }
        catch { Console.Error.WriteLine("Windows could not open the Claude account launcher."); return 4; }
    }
}
