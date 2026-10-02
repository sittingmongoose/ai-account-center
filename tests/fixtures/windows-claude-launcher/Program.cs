using System;
using System.Collections.Generic;
using System.IO;
using System.Text.Json;

public static class WindowsClaudeLauncherOfflineTests
{
    sealed class Catalog : IClaudeRegisteredPackageCatalog
    {
        public IList<ClaudeRegisteredPackage> Packages;
        public int Reads;
        public IList<ClaudeRegisteredPackage> ReadCurrentUserClaudePackages()
        { Reads++; return Packages; }
    }
    static readonly string Roaming = @"C:\Users\fixture\AppData\Roaming";
    static readonly string Old = @"C:\Program Files\WindowsApps\Claude_2.16120.0.0_x64__pzs8sxrjxfjjc";
    static readonly string Current = @"C:\Program Files\WindowsApps\Claude_2.19675.0.0_x64__pzs8sxrjxfjjc";
    static readonly List<string> Checks = new List<string>();
    // Fixture account set: the same shape as the generated file, with an added
    // fifth ID proving new manifest IDs work without source changes.
    static readonly ClaudeAccountSet Accounts = new ClaudeAccountSet(
        new[] { "gmail", "platyr", "party", "me", "added-profile" }, "gmail");
    static ClaudeRegisteredPackage P(string version, string path)
    { return new ClaudeRegisteredPackage("Claude_" + version + "_x64__pzs8sxrjxfjjc", path); }
    static void Check(string name, bool result)
    { if (!result) throw new Exception(name); Checks.Add(name); }
    static bool Throws(Action run)
    { try { run(); return false; } catch (InvalidOperationException) { return true; } }
    static bool ThrowsArgument(Action run)
    { try { run(); return false; } catch (ArgumentException) { return true; } }
    public static int Main()
    {
        foreach (string id in new[] { "platyr", "party", "me", "added-profile" })
        {
            string expectedProfile = Path.Combine(Roaming, "Claude-" + id);
            Catalog original = new Catalog { Packages = new[] { P("2.16120.0.0", Old) } };
            ClaudeLaunchPlan before = ClaudeProfileLaunchPlanner.Create("ccs-claude://launch/" + id,
                @"C:\Windows", Roaming, original, path => path == Path.Combine(Old, "app", "Claude.exe"),
                path => path == expectedProfile, Accounts);
            // Simulate three stale desktop links pointing at Old; the planner
            // intentionally receives no shortcut dependency after Store update.
            Catalog updated = new Catalog { Packages = new[] { P("2.16120.0.0", Old), P("2.19675.0.0", Current) } };
            ClaudeLaunchPlan after = ClaudeProfileLaunchPlanner.Create("ccs-claude://launch/" + id,
                @"C:\Windows", Roaming, updated, path => path == Path.Combine(Current, "app", "Claude.exe"),
                path => path == expectedProfile, Accounts);
            Check(id + " Store update selects existing current executable",
                after.Executable == Path.Combine(Current, "app", "Claude.exe") && after.Executable != before.Executable);
            Check(id + " exact saved profile survives Store update",
                after.ProfilePath == before.ProfilePath && after.ProfilePath == expectedProfile);
            Check(id + " distinct profile argument never becomes default",
                after.Arguments == "--user-data-dir=\"" + expectedProfile + "\"" && after.DefaultAppTarget == null);
            Check(id + " current app working directory selected",
                after.WorkingDirectory == Path.Combine(Current, "app"));
            using (JsonDocument json = JsonDocument.Parse(CcsClaudeAccountLauncher.Describe(after)))
                Check(id + " public describe roundtrip preserves launch plan",
                    json.RootElement.GetProperty("profileId").GetString() == id &&
                    json.RootElement.GetProperty("arguments").GetString() == after.Arguments);
        }
        Catalog unused = new Catalog { Packages = null };
        ClaudeLaunchPlan gmail = ClaudeProfileLaunchPlanner.Create("ccs-claude://launch/" + Accounts.Default, @"C:\Windows",
            Roaming, unused, path => path == @"C:\Windows\explorer.exe", path => false, Accounts);
        Check("Default preserves version-independent Store AUMID", gmail.Arguments == ClaudeProfileLaunchPlanner.DefaultTarget);
        Check("Default does not enumerate packages or require named profile", unused.Reads == 0 && gmail.ProfilePath == null);
        foreach (string invalidRoot in new[] { "", "relative", "C:relative", @"\root-relative" })
        {
            Catalog rootCatalog = new Catalog { Packages = null };
            bool namedRejected = Throws(() => ClaudeProfileLaunchPlanner.Create("ccs-claude://launch/party",
                @"C:\Windows", invalidRoot, rootCatalog,
                path => { throw new Exception("file metadata must not be read"); },
                path => { throw new Exception("directory metadata must not be read"); }, Accounts));
            bool gmailRejected = Throws(() => ClaudeProfileLaunchPlanner.Create("ccs-claude://launch/" + Accounts.Default,
                invalidRoot, Roaming, rootCatalog,
                path => { throw new Exception("file metadata must not be read"); }, path => false, Accounts));
            Check("invalid AppData root rejects before metadata or catalog: " + invalidRoot, namedRejected && rootCatalog.Reads == 0);
            Check("invalid Windows root rejects before default launch: " + invalidRoot, gmailRejected && rootCatalog.Reads == 0);
        }
        Catalog healthy = new Catalog { Packages = new[] { P("2.19675.0.0", Current) } };
        foreach (string invalid in new[] { "ccs-claude://launch/unknown", "ccs-claude://launch/PARTY",
            "ccs-claude://launch/party?command=calc", "ccs-claude://launch/party/", "ccs-claude://launch/../party",
            "ccs-claude://launch/%70arty", "https://launch/party", "ccs-claude://launch/party#me",
            "ccs-claude://launch/bad;id", "ccs-claude://launch/", "ccs-claude://launch/-bad" })
        {
            int reads = healthy.Reads;
            bool rejected = false;
            try { ClaudeProfileLaunchPlanner.Create(invalid, @"C:\Windows", Roaming, healthy,
                    path => { throw new Exception("metadata must not be read"); },
                    path => { throw new Exception("metadata must not be read"); }, Accounts); }
            catch (ArgumentException) { rejected = true; }
            Check("URI allowlist rejects " + invalid, rejected && healthy.Reads == reads);
        }
        Check("missing named profile refuses without default fallback",
            Throws(() => ClaudeProfileLaunchPlanner.Create("ccs-claude://launch/party", @"C:\Windows",
                Roaming, healthy, path => true, path => false, Accounts)));
        Catalog missingNew = new Catalog { Packages = new[] { P("2.16120.0.0", Old), P("2.19675.0.0", Current) } };
        Check("missing current executable refuses obsolete-version fallback",
            Throws(() => ClaudeProfileLaunchPlanner.Create("ccs-claude://launch/party", @"C:\Windows",
                Roaming, missingNew, path => path == Path.Combine(Old, "app", "Claude.exe"), path => true, Accounts)));
        foreach (IList<ClaudeRegisteredPackage> packages in new IList<ClaudeRegisteredPackage>[] {
            null, new ClaudeRegisteredPackage[0],
            new[] { new ClaudeRegisteredPackage("Claude_2.19675.0.0_x64__foreign", Current) },
            new[] { new ClaudeRegisteredPackage("Claude_2.19675.0.0_x64_en-us_pzs8sxrjxfjjc", Current) },
            new[] { new ClaudeRegisteredPackage("Claude_not-a-version_x64__pzs8sxrjxfjjc", Current) },
            new[] { P("2.19675.0.0", "relative") } })
        {
            Check("unavailable/foreign/resource/malformed package refuses " + Checks.Count,
                Throws(() => ClaudeProfileLaunchPlanner.Create("ccs-claude://launch/me", @"C:\Windows",
                    Roaming, new Catalog { Packages = packages }, path => true, path => true, Accounts)));
        }
        Catalog ambiguous = new Catalog { Packages = new[] { P("2.19675.0.0", Current), P("2.19675.0.0", Current + "-other") } };
        Check("equal-version ambiguous registered paths refuse",
            Throws(() => ClaudeProfileLaunchPlanner.Create("ccs-claude://launch/platyr", @"C:\Windows",
                Roaming, ambiguous, path => true, path => true, Accounts)));
        Catalog semantic = new Catalog { Packages = new[] { P("2.9.0.0", Old), P("2.10.0.0", Current) } };
        ClaudeLaunchPlan semanticPlan = ClaudeProfileLaunchPlanner.Create("ccs-claude://launch/me", @"C:\Windows",
            Roaming, semantic, path => true, path => true, Accounts);
        Check("package versions compare numerically", semanticPlan.Executable == Path.Combine(Current, "app", "Claude.exe"));
        // Generated account file: first ID is default, comments/blank lines
        // ignored, every ID validated with the shared safe-ID rule.
        string temp = Path.Combine(Path.GetTempPath(), "ccs-claude-accounts-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(temp);
        try
        {
            File.WriteAllText(Path.Combine(temp, ClaudeProfileLaunchPlanner.AccountsFileName),
                "# generated account list\n\ngmail\nplatyr\nadded-profile\n");
            ClaudeAccountSet loaded = ClaudeProfileLaunchPlanner.LoadAccounts(temp);
            Check("account file loads IDs with first as default",
                loaded.Allowed.Count == 3 && loaded.Default == "gmail" &&
                ClaudeProfileLaunchPlanner.ProfileId("ccs-claude://launch/added-profile", loaded) == "added-profile" &&
                ClaudeProfileLaunchPlanner.ProfileId("ccs-claude://launch/me", loaded) == null);
            foreach (string bad in new[] { "", "# only a comment\n", "gmail\ngmail\n",
                "gmail\nbad;id\n", "ok\n" + new string('x', 65) + "\n", "ok\n-OOPS\n" })
            {
                File.WriteAllText(Path.Combine(temp, ClaudeProfileLaunchPlanner.AccountsFileName), bad);
                Check("account file rejects invalid list", Throws(() => ClaudeProfileLaunchPlanner.LoadAccounts(temp)));
            }
            File.Delete(Path.Combine(temp, ClaudeProfileLaunchPlanner.AccountsFileName));
            Check("missing account file refuses", Throws(() => ClaudeProfileLaunchPlanner.LoadAccounts(temp)));
            Check("null account set cannot resolve URIs",
                ClaudeProfileLaunchPlanner.ProfileId("ccs-claude://launch/gmail", null) == null &&
                ThrowsArgument(() => ClaudeProfileLaunchPlanner.Create("ccs-claude://launch/gmail",
                    @"C:\Windows", Roaming, healthy, path => true, path => true, null)));
        }
        finally { Directory.Delete(temp, true); }
        Console.WriteLine(JsonSerializer.Serialize(new { passed = true, count = Checks.Count, checks = Checks,
            scope = "Actual production launch planner with synthetic current-user package/metadata inputs; no app/task/profile/auth/GUI mutation" }));
        return 0;
    }
}
