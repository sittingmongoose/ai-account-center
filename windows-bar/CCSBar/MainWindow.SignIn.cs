using System;
using System.Globalization;
using System.Linq;
using System.Net;
using System.Reflection;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;

namespace CCSBar;

/// <summary>
/// The sign-in screen's flows (CONTRACT-auth-devices sections 5 to 10, the trays concept's sign-in states): the address
/// check, pairing with the password, first-run setup with the code, the trusted-local-network refusals, the upgrade of
/// a stored version 1 password (Securing), a remote sign-out, Re-pair and Disconnect. Nothing is saved until pair
/// answered 201 and the new key worked once; a working connection is never replaced by one that has not.
/// </summary>
public partial class MainWindow
{
    private SignInView? signInView;
    /// <summary>Bumped by every new flow; a flow's continuations stop when it moved on.</summary>
    private int siRun;
    private CancellationTokenSource? siCancel;
    private Task siTask = Task.CompletedTask;
    /// <summary>Sign in the old way, with the password (version 1): the dashboard predates pairing, or pairing is turned
    /// off and the owner chose the password option.</summary>
    private bool siLegacy;
    /// <summary>The dashboard's last /api/auth/check said it can pair trays (its words after a password sign-in).</summary>
    private bool siSupportsPairing;
    /// <summary>
    /// From the moment a pair request is sent until its key is saved, adopted or refused. The dashboard replaces this
    /// install's record as soon as it answers 201, which revokes a Re-pair's current key before the new key is ever
    /// used, so nothing may walk away from the answer: Cancel and Escape rest, and Settings' Re-pair and Disconnect wait.
    /// </summary>
    private bool pairHeld;
    internal bool PairHeld => pairHeld;
    private DateTimeOffset lastRotateAttempt = DateTimeOffset.MinValue, lastDeviceRead = DateTimeOffset.MinValue;
    /// <summary>The time of the last good sample, for the signed-out note.</summary>
    private DateTimeOffset? lastGoodSample;

    internal SignInView SignIn => signInView ??= CreateSignIn();
    /// <summary>The running sign-in flow (the checks and the E2E driver await it).</summary>
    internal Task SignInFlowTask => siTask;
    internal bool SignInVisible => signInVisible;
    /// <summary>How long the address check waits for an answer.</summary>
    internal TimeSpan AddressTimeout { get; set; } = TimeSpan.FromSeconds(10);
    /// <summary>The pause on the success state before the hand-off (shortened by the checks).</summary>
    internal TimeSpan SuccessPause { get; set; } = TimeSpan.FromSeconds(1);
    /// <summary>The pace of the visible step ticks (shortened by the checks).</summary>
    internal TimeSpan StepPace { get; set; } = TimeSpan.FromMilliseconds(220);
    /// <summary>The pauses before a new key's devices/me is asked again after a network error (shortened by the checks).</summary>
    internal TimeSpan[] KeyProofBackoff { get; set; } = { TimeSpan.FromSeconds(1), TimeSpan.FromSeconds(3) };

    private static string AppVersion => Assembly.GetExecutingAssembly().GetName().Version?.ToString(3) ?? "1.0.0";

    private SignInView CreateSignIn()
    {
        var view = new SignInView();
        view.Submitted += SiSubmit;
        view.AltClicked += SiAlt;
        view.ChangeClicked += () => { view.Addr.Value = view.Model.Verified; view.Model.Note = null; view.Apply(SignInState.FirstRun); view.FocusFor(SignInState.FirstRun); };
        view.UseLocalClicked += () => { if (view.Model.LocalAddress is { Length: > 0 } local) { view.Addr.Value = local; Run(SiContinue); } };
        view.CountdownFinished += () => { view.Model.TriesLeft = 5; view.Apply(SignInState.Password); view.FocusFor(SignInState.Password); };
        view.UsePasswordClicked += () =>
        {
            if (pairHeld || !view.Model.PasswordFallback) return;
            siLegacy = true; view.Model.Legacy = true; view.Model.Note = null;
            view.Apply(SignInState.Password); view.FocusFor(SignInState.Password);
        };
        SignInLayer.Children.Clear();
        SignInLayer.Children.Add(view);
        return view;
    }

    /// <summary>What the header, the footer note and the tray tooltip say while the sign-in screen shows.</summary>
    internal string? SignInStatus
    {
        get
        {
            if (!signInVisible || signInView is null) return client is null ? (connection?.IsSignedOut == true && connection.SignedOutReason != "disconnected" ? "Signed out" : "Not paired") : null;
            if (signInView.Model.Repair) return pairHeld ? "Re-pairing" : "Re-pairing · the current key still works";
            if (signInView.Model.Upgrade && !signInView.Model.Legacy) return pairHeld ? "Pairing" : "Pairing · the saved password still works";
            return signInView.Model.State switch
            {
                SignInState.Pairing => "Pairing", SignInState.Securing => "Securing this tray", SignInState.Success => "Paired",
                SignInState.SignedOut or SignInState.SignedOutAll or SignInState.Expired => "Signed out", _ => "Not paired",
            };
        }
    }

    private string SignInFootNote => signInView?.Model switch
    {
        { HasCurrent: true } when pairHeld => "Pairing; this finishes by itself",
        { Repair: true } => "Cancel keeps the current device key and returns to usage",
        { Upgrade: true } => "Cancel keeps the saved password and returns to usage",
        { State: SignInState.Securing } => "This runs once, by itself",
        { State: SignInState.Success } => "Opening your accounts",
        _ => "Usage appears after this tray is paired",
    };

    /// <summary>What a launch shows for the stored file: a signed-out version 2 file opens on its sign-out screen
    /// (Disconnect on the first run with the address filled in), and no file opens on the first run.</summary>
    internal void ShowSignInForStoredConnection() => ShowSignIn(connection is { IsSignedOut: true } stub ? StateFor(stub) : SignInState.FirstRun);

    /// <summary>Settings › Change, or the first run. Kept for the older call sites.</summary>
    public void ShowSignIn(bool firstRun) => ShowSignIn(firstRun || connection is null ? SignInState.FirstRun : SignInState.Password, repair: client is not null);

    /// <summary>Shows the sign-in screen in place of the account list, in a state, with the dashboard's entrance.</summary>
    public void ShowSignIn(SignInState state, bool repair = false, bool entrance = true)
    {
        CancelConnectionCheck();
        siCancel?.Cancel();
        siRun++;
        var view = SignIn;
        var model = view.Model;
        model.Repair = repair && client is { Paired: true };
        model.Upgrade = repair && client is { Paired: false };
        model.Legacy = false; model.PasswordFallback = false; model.KeyUncertain = false;
        model.Note = null; model.NoteField = null; model.Peer = null; model.Tried = ""; model.TriesLeft = 5; model.Step = 0; model.Flow = SignInFlow.Pair;
        model.Verified = connection?.BaseURL is { Length: > 0 } saved ? Origin(saved) : "";
        model.LocalAddress = model.Verified.Length > 0 && Uri.TryCreate(model.Verified, UriKind.Absolute, out var local) && LocalNetwork.IsLocalHostName(local.Host) ? model.Verified : null;
        model.Username = connection?.Username ?? "";
        model.DeviceName = Environment.MachineName;
        model.DisconnectedAt = connection?.SignedOutReason == "disconnected" && DateTimeOffset.TryParse(connection.SignedOutAt, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var gone) ? gone : null;
        model.SignedOutAt = connection?.SignedOutReason is { } reason && reason != "disconnected" && DateTimeOffset.TryParse(connection.SignedOutAt, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var at) ? at : null;
        model.RevokedBy = connection?.RevokedBy;
        model.LastSample = lastGoodSample;
        siLegacy = false;
        view.Addr.Value = model.Verified;
        view.User.Value = model.Username;
        view.Pass.Clear(); view.Confirm.Clear(); view.Code.Clear();
        view.Pass.ToggleShown(false); view.Confirm.ToggleShown(false);
        view.Apply(state, instant: true);
        signInVisible = true;
        SignInLayer.BeginAnimation(OpacityProperty, null);
        SignInLayer.Opacity = 1;
        SignInLayer.Visibility = Visibility.Visible;
        view.PlayEntrance(entrance && IsVisible);
        signInEntered = entrance && IsVisible;
        RenderFooter(); UpdateStatus(); SampleChanged?.Invoke();
        Dispatcher.BeginInvoke(new Action(() => { if (signInVisible) view.FocusFor(view.Model.State); }), DispatcherPriority.Input);
    }

    private void CloseSignIn()
    {
        signInVisible = false;
        siCancel?.Cancel();
        siRun++;
        signInView?.StopTimers();
        Motion.To(SignInLayer, OpacityProperty, 0, 200, completed: (_, _) => { if (!signInVisible) SignInLayer.Visibility = Visibility.Collapsed; });
        RenderFooter(); UpdateStatus(); SampleChanged?.Invoke();
    }

    private static string Origin(string address) => Uri.TryCreate(address.Trim(), UriKind.Absolute, out var uri) ? uri.GetLeftPart(UriPartial.Authority) : address.Trim();

    private void Run(Func<int, CancellationToken, Task> flow)
    {
        siCancel?.Cancel();
        siCancel = new CancellationTokenSource();
        var run = ++siRun;
        siTask = Guard(flow(run, siCancel.Token));
    }

    private static async Task Guard(Task flow)
    {
        try { await flow; }
        catch (OperationCanceledException) { }
    }

    private bool Alive(int run) => run == siRun && signInVisible;

    /// <summary>Enter or the primary: what it does depends on the state, as in the concept's siSubmit.</summary>
    private void SiSubmit()
    {
        var view = SignIn;
        view.Model.Note = null; view.Model.NoteField = null;
        switch (view.Model.State)
        {
            case SignInState.Pairing or SignInState.Securing or SignInState.Success or SignInState.RateLimited: return;
            case SignInState.FirstRun or SignInState.NotLocal or SignInState.Unreachable or SignInState.WrongAddress: Run(SiContinue); break;
            case SignInState.PairingOff: Run(SiRecheck); break;
            case SignInState.SetupCode: Run(SiSetup); break;
            default: Run(SiPair); break;
        }
    }

    /// <summary>Cancel (Re-pair, Pair or Change from Settings) and "Keep current connection": the current connection keeps
    /// working. Never once the pair request is out: its answer may already have replaced the current key.</summary>
    private void SiAlt()
    {
        if (pairHeld || !SignIn.Model.HasCurrent || client is null) return;
        var uncertain = SignIn.Model.KeyUncertain;
        CloseSignIn();
        if (!uncertain) { FlashStatus("The current connection was kept."); return; }
        FlashStatus("Back to usage. The next refresh shows whether the current key still works.");
        _ = Refresh(true);
    }

    /// <summary>Holds (or releases) a pair request's answer: see <see cref="pairHeld"/>.</summary>
    private void HoldPair(bool held)
    {
        pairHeld = held;
        if (signInView is not null) signInView.AltHeld = held;
        RenderFooter(); UpdateStatus(); SampleChanged?.Invoke();
    }

    /// <summary>Applies a state with a message that is not the state's own (a refusal with no screen of its own).</summary>
    private void SiNote(SignInState state, string bold, string? sub, string? field = null, bool info = false)
    {
        var view = SignIn;
        view.Model.Note = (bold, sub, info); view.Model.NoteField = field;
        view.Apply(state);
        view.Model.Note = null; view.Model.NoteField = null;
        if (field is not null) view.Shake();
    }

    // ------------------------------------------------------------------ 1 · the address

    /// <summary>Continue: the address gets http:// when it has no scheme; the tray's own check refuses a public
    /// address before anything is sent; then GET /api/auth/check (10 s) decides the next state. Nothing is saved.</summary>
    private async Task SiContinue(int run, CancellationToken cancel)
    {
        var view = SignIn;
        var raw = view.Addr.Value.Trim();
        if (raw.Length == 0) { view.Fail("addr", "Enter the dashboard address."); return; }
        if (!raw.Contains("://", StringComparison.Ordinal)) raw = "http://" + raw;
        if (!Uri.TryCreate(raw, UriKind.Absolute, out var uri) || uri.Scheme is not ("http" or "https") || !string.IsNullOrEmpty(uri.UserInfo)
            || uri.AbsolutePath != "/" || !string.IsNullOrEmpty(uri.Query) || !string.IsNullOrEmpty(uri.Fragment))
        { view.Fail("addr", "Enter the dashboard's address without a path, for example http://192.168.1.20:3000."); return; }
        var origin = uri.GetLeftPart(UriPartial.Authority);
        view.Addr.Value = origin;
        view.Model.Tried = origin;
        view.Model.Peer = null;
        view.SetBusy(true);
        if (uri.Scheme == "http")
        {
            // Plain HTTP carries the password: only to an address on the local network, checked before asking anything.
            var local = await LocalNetwork.ResolvesLocally(uri.Host, TimeSpan.FromSeconds(5));
            if (!Alive(run)) return;
            if (local is null) { view.Model.TimedOut = false; view.Apply(SignInState.Unreachable); return; }
            if (local == false) { view.Apply(SignInState.NotLocal); return; }
        }
        await SiAsk(run, new Uri(origin + "/"), cancel);
    }

    /// <summary>GET /api/auth/check at an address and the state it leads to.</summary>
    private async Task SiAsk(int run, Uri origin, CancellationToken cancel)
    {
        var view = SignIn;
        AuthCheck check;
        using (var api = new AuthApi(origin, AddressTimeout))
        {
            try { check = await api.Check(cancel); }
            catch (DashboardReachException failure)
            {
                if (!Alive(run)) return;
                view.Model.Tried = origin.GetLeftPart(UriPartial.Authority);
                view.Model.TimedOut = failure.TimedOut;
                view.Apply(failure.NotDashboard ? SignInState.WrongAddress : SignInState.Unreachable);
                view.FocusFor(view.Model.State);
                return;
            }
        }
        if (!Alive(run)) return;
        view.Model.Verified = origin.GetLeftPart(UriPartial.Authority);
        Decide(check);
    }

    /// <summary>The state an answer from /api/auth/check leads to (CLIENT API SHEET 4.1, the corrected tray copy).</summary>
    private void Decide(AuthCheck check)
    {
        var view = SignIn;
        siLegacy = !check.SupportsPairing;
        siSupportsPairing = check.SupportsPairing;
        view.Model.Legacy = siLegacy;
        // Pairing turned off: a tray without a device key may sign in with its password instead (never a paired one).
        view.Model.PasswordFallback = !view.Model.Repair && client is not { Paired: true };
        SignInState next;
        if (check.AccessMode == "open")
        {
            SiNote(SignInState.FirstRun, "This dashboard has no sign-in turned on.", "Turn on dashboard sign-in first; a tray pairs with its username and password.", "addr");
            return;
        }
        if (siLegacy) next = check.AccessMode == "setup" ? SignInState.FirstRun : SignInState.Password;
        else if (check.CanPair) next = check.AccessMode == "setup" ? SignInState.SetupCode : SignInState.Password;
        else if (check.TrustedLocalNetwork == false) next = SignInState.PairingOff;
        else { view.Model.Peer = check.Connection?.Peer is { Length: > 0 and <= 64 } peer ? peer : null; next = SignInState.NotLocal; }
        if (siLegacy && check.AccessMode == "setup")
        {
            SiNote(SignInState.FirstRun, "This dashboard has no sign-in yet.", "Set it up in a browser on the dashboard computer, then pair this tray.", "addr");
            return;
        }
        view.Apply(next);
        view.FocusFor(next);
    }

    /// <summary>State 5's Try again: asks the dashboard once more.</summary>
    private async Task SiRecheck(int run, CancellationToken cancel)
    {
        var view = SignIn;
        view.SetBusy(true);
        await SiAsk(run, new Uri(view.Model.Verified + "/"), cancel);
    }

    // ------------------------------------------------------------------ pairing with the password

    private async Task SiPair(int run, CancellationToken cancel)
    {
        var view = SignIn;
        var username = view.User.Value.Trim();
        var password = view.Pass.Value;
        if (username.Length == 0 || password.Length == 0) { view.Fail(username.Length == 0 ? "user" : "pass", "Enter your dashboard username and password."); return; }
        if (view.Model.Verified.Length == 0) { view.Apply(SignInState.FirstRun); return; }
        var origin = new Uri(view.Model.Verified + "/");
        if (siLegacy) { await SiLegacy(run, origin, username, password); return; }
        view.SetBusy(true);
        using var api = new AuthApi(origin, TimeSpan.FromSeconds(20));
        var current = view.Model.Repair ? client : null;
        HoldPair(true);
        try
        {
            AuthAnswer answer;
            // Not cancellable: the dashboard acts on the request the moment it arrives.
            try { answer = await api.Pair(username, password, Environment.MachineName, lastInstallId = InstallIdFor(), AppVersion, CancellationToken.None); }
            catch (DashboardReachException failure)
            {
                await SiPairLost(run, failure, current);
                return;
            }
            await SiPairAnswer(run, origin, username, password, answer, SignInFlow.Pair, cancel);
        }
        finally { HoldPair(false); }
    }

    /// <summary>
    /// The pair request got no answer. With no connection made, it never arrived. Timed out or cut off, the dashboard may
    /// have acted on it, and for a Re-pair that would have replaced the current key: the current key is asked once.
    /// Still working, the screen says the connection is unchanged; refused, this tray is signed out (its new key never
    /// arrived) and says so; no answer either, the screen says it can't tell.
    /// </summary>
    private async Task SiPairLost(int run, DashboardReachException failure, DashboardClient? current)
    {
        var survived = !failure.NeverSent && current is not null ? await StillWorks(current) : true;
        if (!Alive(run)) return;
        var view = SignIn;
        if (survived == false)
        {
            SignedOutRemotely(new DeviceSignedOutException("device_revoked"));
            SiNote(SignIn.Model.State, "Pairing reached the dashboard, but its answer was lost.", "The dashboard replaced this tray's key. Pair again.");
            return;
        }
        view.Model.KeyUncertain = survived is null;
        view.Model.Tried = view.Model.Verified; view.Model.TimedOut = failure.TimedOut;
        view.Addr.Value = view.Model.Verified;
        view.Apply(SignInState.Unreachable);
    }

    /// <summary>Whether a key this tray already holds still works: true, false (a 401 device code), or null (no answer
    /// within the address check's time limit).</summary>
    private async Task<bool?> StillWorks(DashboardClient current)
    {
        if (!current.Paired) return true;
        var probe = current.DeviceMe();
        if (await Task.WhenAny(probe, Task.Delay(AddressTimeout)) != probe)
        {
            _ = probe.ContinueWith(late => late.Exception, TaskContinuationOptions.OnlyOnFaulted);
            return null;
        }
        try { await probe; return true; }
        catch (DeviceSignedOutException signedOut) when (signedOut.IsDeviceCode) { return false; }
        catch { return null; }
    }

    /// <summary>The installId this tray pairs with: the one it already holds (so pairing again replaces that record and
    /// revokes its old key), else the one this session already sent (a retry replaces an attempt whose key was never
    /// saved, instead of leaving it behind), else a new random one.</summary>
    private string InstallIdFor() =>
        connection?.InstallId is { Length: 36 } kept && Guid.TryParse(kept, out _) ? kept
        : lastInstallId is { Length: 36 } sent && Guid.TryParse(sent, out _) ? sent
        : Guid.NewGuid().ToString("D");

    /// <summary>What pair's answer leads to. A 201 is always finished (verified, saved and adopted), even when the screen
    /// moved on meanwhile: the dashboard has already replaced this install's record. Any other answer is shown only
    /// while its flow is still the one on screen.</summary>
    private async Task SiPairAnswer(int run, Uri origin, string username, string password, AuthAnswer answer, SignInFlow flow, CancellationToken cancel)
    {
        var view = SignIn;
        if (answer.Status != HttpStatusCode.Created && !Alive(run)) return;
        if (Alive(run)) view.SetBusy(false);
        switch ((int)answer.Status)
        {
            case 201:
                if (PairedDevice.From(answer) is not { } device) { if (Alive(run)) SiNote(SignInState.Password, "The dashboard sent an unreadable device key.", "Nothing was saved. Try again."); return; }
                await SiFinish(run, origin, username, device, flow);
                return;
            case 401 when answer.Code == "invalid_credentials":
                var tries = answer.Int("triesLeft") ?? Math.Max(0, view.Model.TriesLeft - 1);
                view.Model.TriesLeft = tries;
                if (tries <= 0) { RateLimit(TimeSpan.FromMinutes(15)); return; }
                view.Apply(SignInState.WrongPassword);
                view.FocusFor(SignInState.WrongPassword);
                return;
            case 429:
                RateLimit(answer.RetryAfter ?? TimeSpan.FromMinutes(15));
                return;
            case 403 when answer.Code == "secure_transport_required":
                // The dashboard does not trust this connection: read why (switch off, or not a local peer).
                view.SetBusy(true);
                await SiAsk(run, origin, cancel);
                return;
            case 404 or 405:
                // A dashboard that predates pairing: keep working the old way (contract section 8).
                siLegacy = true; siSupportsPairing = false; view.Model.Legacy = true;
                await SiLegacy(run, origin, username, password);
                return;
            case 409 when answer.Code == "too_many_devices":
                SiNote(view.Model.State, "Twenty trays are already paired with this dashboard.", "Revoke one under Settings › Dashboard sign-in, then try again.");
                return;
            case 409 when answer.Code == "auth_not_configured":
                view.SetBusy(true);
                await SiAsk(run, origin, cancel);
                return;
            case 503:
                SiNote(view.Model.State, "The dashboard can't save paired trays right now.", "Try again in a moment.");
                return;
            default:
                SiNote(view.Model.State, "Pairing failed on the dashboard.", "Try again.");
                return;
        }
    }

    private void RateLimit(TimeSpan wait)
    {
        var view = SignIn;
        if (wait <= TimeSpan.Zero) wait = TimeSpan.FromMinutes(15);
        view.Model.LimitTotal = wait;
        view.Model.LimitUntil = DateTimeOffset.UtcNow + wait;
        view.Model.TriesLeft = 0;
        view.Pass.Clear();
        view.Apply(SignInState.RateLimited);
    }

    /// <summary>
    /// After pair 201: the steps tick through while the tray proves the new key works (GET /api/auth/devices/me, asked
    /// again after a network error), then saves it with DPAPI (version 2, no password), forgets the password and swaps
    /// in the paired client. Success, then the hand-off into the account list.
    /// The dashboard answered 201, so it has already replaced this install's record, and a Re-pair's current key stopped
    /// working with that answer. From here the new key is therefore proved, saved and adopted to the end whatever the
    /// screen does meanwhile; only the visible steps follow the screen. When the new key gets no answer, a Re-pair asks
    /// whether the current key survived: if it did (another dashboard answered the pair), nothing is saved; if it did not,
    /// the new key is the only one that can work, so it is saved and used, and the next refresh proves it.
    /// </summary>
    private async Task SiFinish(int run, Uri origin, string username, PairedDevice device, SignInFlow flow)
    {
        var view = SignIn;
        var current = view.Model.Repair && client is { Paired: true } held ? held : null;
        bool Shown() => Alive(run);
        if (Shown())
        {
            view.Model.Flow = flow; view.Model.Username = username; view.Model.Step = 0;
            view.Pass.Clear(); view.Confirm.Clear();
            view.Apply(SignInState.Pairing);
        }
        await Task.Delay(StepPace); if (Shown()) view.SetStep(1);
        await Task.Delay(StepPace); if (Shown()) view.SetStep(2);
        var candidate = new ConnectionSettings
        {
            Version = 2, BaseURL = origin.GetLeftPart(UriPartial.Authority), Username = username, DeviceId = device.DeviceId, DeviceToken = device.Token,
            // The installId that was sent with this pairing, so pairing again replaces this record.
            InstallId = lastInstallId ?? InstallIdFor(), PairedAt = device.PairedAt ?? DateTimeOffset.UtcNow.ToString("O"), RotateAfter = device.RotateAfter,
        };
        var verified = new DashboardClient(candidate);
        var (record, failure) = await ProveKey(verified);
        var unproved = false;
        if (record is not null) candidate.RotateAfter = record.RotateAfter ?? candidate.RotateAfter;
        else
        {
            var refused = failure is DeviceSignedOutException;
            // Only a Re-pair has a key to lose; another dashboard answering the pair leaves it working.
            var survived = current is null ? true : await StillWorks(current);
            var replaced = survived == false || survived is null && ConnectionSettings.SameOrigin(candidate.BaseURL, current!.BaseURL.ToString());
            if (!replaced)
            {
                verified.Dispose();
                if (!Shown()) return;
                if (refused) SiNote(SignInState.Password, "The new device key did not work.", current is null ? "Nothing was saved. Try pairing again." : "Nothing was saved, and this tray keeps its current key. Try pairing again.");
                else { view.Model.KeyUncertain = survived is null; view.Model.Tried = view.Model.Verified; view.Model.TimedOut = failure is TaskCanceledException; view.Apply(SignInState.Unreachable); }
                return;
            }
            if (refused)
            {
                // The new key was refused and the current one is gone: this tray is signed out, and says why.
                verified.Dispose();
                SignedOutRemotely(new DeviceSignedOutException("device_revoked"));
                SiNote(SignIn.Model.State, "The new device key did not work.", "The dashboard had already replaced the current one. Pair again.");
                return;
            }
            unproved = true;
        }
        var saved = true;
        try { SecureStore.Save(candidate, ConnectionStorePath); }
        catch (Exception)
        {
            if (current is null)
            {
                verified.Dispose();
                if (Shown()) SiNote(SignInState.Password, "Windows could not save the device key.", "Nothing was changed. Try again.");
                return;
            }
            // The current key is already gone: the new one is used for this session.
            saved = false;
        }
        try { SecureStore.DeleteRollback(ConnectionStorePath); } catch { }
        if (Shown()) view.SetStep(3);
        await Task.Delay(StepPace);
        if (Shown()) view.SetStep(4);
        AdoptClient(candidate, verified);
        var done = flow == SignInFlow.Secure ? "Secured · this tray now signs in with a device key" : "Paired · signed in with a device key";
        if (saved && !unproved && Shown()) { await SiSucceed(run, done); return; }
        // Not proved yet, not saved, or the screen moved on: no success screen; the list shows what a refresh finds (a
        // 401 there signs the tray out, with the signed-out screen).
        if (Shown()) CloseSignIn();
        await Refresh(true);
        if (!ReferenceEquals(client, verified)) return;
        FlashStatus(!saved ? "Paired for this session. Windows could not save the device key, so pair again after a restart."
            : dashboard is not null && !staleSample ? done
            : "Paired. The dashboard isn't answering yet, so the new key is checked at the next refresh.");
    }

    /// <summary>GET /api/auth/devices/me with a new key, asked again after a network error (<see cref="KeyProofBackoff"/>):
    /// the device record, or the failure (a 401 device code refused the key).</summary>
    private async Task<(DeviceRecord? Record, Exception? Failure)> ProveKey(DashboardClient keyed)
    {
        for (int attempt = 0; ; attempt++)
        {
            try { return (await keyed.DeviceMe(), null); }
            catch (DeviceSignedOutException refused) { return (null, refused); }
            catch (Exception failure)
            {
                if (attempt >= KeyProofBackoff.Length) return (null, failure);
                await Task.Delay(KeyProofBackoff[attempt]);
            }
        }
    }

    private string? lastInstallId;

    /// <summary>The verified connection replaces the running one; a sample or an Open from the old one is dropped.</summary>
    private void AdoptClient(ConnectionSettings candidate, DashboardClient verified)
    {
        var previous = client;
        connection = candidate; client = verified;
        connectionGeneration++;
        dashboard = null; staleSample = false; lastFailure = DateTimeOffset.MinValue; statusFlash = null;
        lastRotateAttempt = DateTimeOffset.MinValue; lastDeviceRead = DateTimeOffset.UtcNow;
        openProgress.Clear();
        if (!ReferenceEquals(previous, verified)) previous?.Dispose();
    }

    /// <summary>Success: the button turns calm with its check and the bar fills; the list loads underneath, then the
    /// screen fades away and the list staggers in with every meter sweeping from 0.</summary>
    private async Task SiSucceed(int run, string flash)
    {
        var view = SignIn;
        view.Apply(SignInState.Success);
        RenderFooter(); UpdateStatus(); SampleChanged?.Invoke();
        var load = Refresh(true);
        await Task.WhenAll(load, Task.Delay(SuccessPause));
        if (run != siRun) return;
        Handoff(flash);
    }

    /// <summary>The hand-off (the header never moves): the sign-in screen fades and lifts away in 340 ms, then the
    /// list loads in with the first-open stagger while every meter sweeps from 0, ease-out and never past its reading.</summary>
    private void Handoff(string flash)
    {
        signInVisible = false;
        siRun++;
        var view = SignIn;
        view.StopTimers();
        var items = ContentPanel.Children.OfType<FrameworkElement>().ToList();
        foreach (var item in items) item.Opacity = 0;
        view.Leave(() =>
        {
            if (signInVisible) return;
            SignInLayer.Visibility = Visibility.Collapsed;
            for (int i = 0; i < items.Count; i++)
            {
                var item = items[i];
                if (item.RenderTransform is not TranslateTransform shift) item.RenderTransform = shift = new TranslateTransform();
                Motion.To(item, OpacityProperty, 1, 240, delay: 40 + Math.Min(i, 26) * 26, from: 0);
                Motion.To(shift, TranslateTransform.YProperty, 0, 380, delay: 40 + Math.Min(i, 26) * 26, from: 6);
            }
            foreach (var meter in meters.Values)
            {
                if (meter.Target is not double target || target <= 0) continue;
                meter.BeginAnimation(Meter.ShownProperty, null); meter.Shown = 0;
                Motion.To(meter, Meter.ShownProperty, target, Motion.Draw, Motion.Out, delay: 180, from: 0);
            }
            Dispatcher.BeginInvoke(new Action(() => { PlacePlatters(); UpdateFade(); }), DispatcherPriority.Loaded);
        });
        FlashStatus(flash);
        RenderFooter(); SampleChanged?.Invoke();
    }

    // ------------------------------------------------------------------ 2 · first-run setup with the code

    private async Task SiSetup(int run, CancellationToken cancel)
    {
        var view = SignIn;
        var username = view.User.Value.Trim();
        var password = view.Pass.Value;
        var problem = SetupProblem(username, password, view.Confirm.Value, view.Code.Value.Trim());
        if (problem is { } bad) { view.Fail(bad.Field, bad.Text); return; }
        var origin = new Uri(view.Model.Verified + "/");
        view.SetBusy(true);
        using var api = new AuthApi(origin, TimeSpan.FromSeconds(20));
        AuthAnswer answer;
        try { answer = await api.Setup(username, password, view.Code.Value.Trim().ToUpperInvariant(), cancel); }
        catch (DashboardReachException failure)
        {
            if (!Alive(run)) return;
            view.Model.Tried = view.Model.Verified; view.Model.TimedOut = failure.TimedOut;
            view.Apply(SignInState.Unreachable);
            return;
        }
        if (!Alive(run)) return;
        view.SetBusy(false);
        switch ((int)answer.Status)
        {
            case 201:
                view.Model.Username = username;
                view.SetBusy(true);
                HoldPair(true);
                try
                {
                    AuthAnswer paired;
                    try { paired = await api.Pair(username, password, Environment.MachineName, lastInstallId = InstallIdFor(), AppVersion, CancellationToken.None); }
                    catch (DashboardReachException) { if (Alive(run)) SiNote(SignInState.Password, "The sign-in was created, but pairing could not finish.", "Sign in with it to pair this tray."); return; }
                    await SiPairAnswer(run, origin, username, password, paired, SignInFlow.Setup, cancel);
                }
                finally { HoldPair(false); }
                return;
            case 403 when answer.Code == "setup_code_required": view.Fail("code", "Enter the setup code from the server's terminal."); return;
            case 403 when answer.Code == "setup_code_invalid":
                var left = answer.Int("triesLeft");
                if (left is 0) { RateLimit(TimeSpan.FromMinutes(15)); return; }
                view.Fail("code", "That setup code isn't right." + (left is int n ? $" {n} {(n == 1 ? "try" : "tries")} left." : ""));
                return;
            case 403 when answer.Code == "secure_transport_required": view.SetBusy(true); await SiAsk(run, origin, cancel); return;
            case 409 when answer.Code == "already_configured": SiNote(SignInState.Password, "This dashboard already has a sign-in.", "Sign in with it to pair this tray."); return;
            case 409 when answer.Code == "managed_by_env": SiNote(SignInState.SetupCode, "This dashboard's sign-in is set by its environment.", "Set it there, then pair with it."); return;
            case 400 when answer.Code == "invalid_username": view.Fail("user", "Start the username with a letter; use 3 or more letters, numbers, - or _."); return;
            case 400 when answer.Code == "weak_password": view.Fail("pass", answer.Text("reason") == "too_long" ? "Keep the password within 72 bytes." : "Use at least 8 characters for the password."); return;
            case 429: RateLimit(answer.RetryAfter ?? TimeSpan.FromMinutes(15)); return;
            default: SiNote(SignInState.SetupCode, "The sign-in could not be created.", "Try again."); return;
        }
    }

    /// <summary>The concept's setup validation, before anything is sent.</summary>
    internal static (string Field, string Text)? SetupProblem(string username, string password, string confirm, string code)
    {
        if (!System.Text.RegularExpressions.Regex.IsMatch(username, "^[A-Za-z][A-Za-z0-9_-]{2,63}$")) return ("user", "Start the username with a letter; use 3 or more letters, numbers, - or _.");
        if (new StringInfo(password).LengthInTextElements < 8) return ("pass", "Use at least 8 characters for the password.");
        if (System.Text.Encoding.UTF8.GetByteCount(password) > 72) return ("pass", "Keep the password within 72 bytes.");
        if (confirm != password) return ("confirm", "The confirmation doesn't match the password.");
        if (code.Length == 0) return ("code", "Enter the setup code from the server's terminal.");
        if (code.Count(char.IsAsciiLetterOrDigit) != 8 || code.Any(c => !char.IsAsciiLetterOrDigit(c) && c != '-')) return ("code", "That setup code isn't right. It has 8 letters and digits.");
        return null;
    }

    // ------------------------------------------------------------------ a dashboard that predates pairing

    /// <summary>The old way, kept for a dashboard without pairing: verify the password sign-in, then save it (version
    /// 1), exactly as Change did before pairing (<see cref="SubmitConnection"/>).</summary>
    private async Task SiLegacy(int run, Uri origin, string username, string password)
    {
        var view = SignIn;
        view.SetBusy(true);
        var failure = await SubmitConnection(origin.GetLeftPart(UriPartial.Authority), username, password);
        if (!Alive(run)) return;
        view.SetBusy(false);
        if (failure is not null) { SiNote(SignInState.Password, failure, null, "pass"); return; }
        view.Pass.Clear();
        signInVisible = false; siRun++;
        view.StopTimers();
        SignInLayer.Visibility = Visibility.Collapsed;
        RenderFooter(); UpdateStatus(); SampleChanged?.Invoke();
        await Refresh(true);
        FlashStatus(siSupportsPairing ? "Signed in with your password. Turn on Trust this local network to pair this tray." : "Signed in with your password. This dashboard can't pair trays yet.");
    }

    // ------------------------------------------------------------------ 9 · securing: upgrade a stored version 1 password

    /// <summary>
    /// On launch with a version 1 connection (a stored password, no key), at most once per launch and every 24 hours:
    /// when the dashboard can pair this computer, copy the file to the rollback, pair with the stored credentials, write
    /// version 2 without the password, then prove the key with GET /api/auth/devices/me. On 200 the rollback and the
    /// password are deleted; on a 401 device code version 1 comes back from the rollback and the cookie sign-in stays; on
    /// a network error both are kept and the check runs again on the next poll (the rollback lives at most 24 hours).
    /// A dashboard without pairing, or one that does not trust this connection, leaves version 1 working as it is.
    /// </summary>
    internal async Task MigrateIfDue(bool force = false)
    {
        if (connection is not { HasPassword: true, IsPaired: false } legacy || client is null) return;
        var now = DateTimeOffset.UtcNow;
        if (!force && DateTimeOffset.TryParse(preferences.LastPairAttempt, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var last) && now - last < TimeSpan.FromHours(24)) return;
        Uri origin;
        try { origin = legacy.ValidateAddress(); } catch { return; }
        AuthCheck check;
        using (var api = new AuthApi(origin, AddressTimeout))
        {
            try { check = await api.Check(); }
            catch (DashboardReachException) { return; } // offline now: try again at the next launch
        }
        if (!check.SupportsPairing || !check.CanPair || check.AccessMode != "login")
        {
            // No pairing on this dashboard, or it does not trust this connection yet: keep version 1 for now.
            preferences.LastPairAttempt = now.ToString("O"); preferences.Save();
            return;
        }
        preferences.LastPairAttempt = now.ToString("O"); preferences.Save();
        ShowSignIn(SignInState.Securing, entrance: IsVisible);
        HoldPair(true);
        try { await Secure(legacy, origin, now); }
        finally { HoldPair(false); }
    }

    /// <summary>Migration steps 1 to 4 (<see cref="MigrateIfDue"/>), from the rollback copy to the first request with the
    /// new key; held like any pair request, so nothing walks away from its answer.</summary>
    private async Task Secure(ConnectionSettings legacy, Uri origin, DateTimeOffset now)
    {
        var view = SignIn;
        view.Model.Flow = SignInFlow.Secure;
        var run = siRun;
        var path = ConnectionStorePath;
        // 1. The rollback copy (same bytes, same DPAPI scope and entropy).
        try { SecureStore.KeepRollback(path); }
        catch { CloseSignIn(); return; }
        view.SetStep(1);
        // 2. Pair with the stored credentials.
        AuthAnswer answer;
        using (var api = new AuthApi(origin, TimeSpan.FromSeconds(20)))
        {
            try { answer = await api.Pair(legacy.Username, legacy.Password!, Environment.MachineName, lastInstallId = InstallIdFor(), AppVersion); }
            catch (DashboardReachException) { SecureStore.DeleteRollback(path); if (run == siRun) CloseSignIn(); return; }
        }
        if (PairedDevice.From(answer) is not { } device)
        {
            SecureStore.DeleteRollback(path);
            if (run != siRun) return;
            if (answer.Status == HttpStatusCode.Unauthorized && answer.Code == "invalid_credentials")
            {
                // The stored password no longer works: no retry; the pairing screen with the username filled in.
                ShowSignIn(SignInState.Password);
                SiNote(SignInState.Password, "The saved password no longer works.", "Sign in to pair this tray.");
                return;
            }
            CloseSignIn();
            return;
        }
        // 3. Version 2 replaces version 1: the key, no password.
        var paired = new ConnectionSettings
        {
            Version = 2, BaseURL = origin.GetLeftPart(UriPartial.Authority), Username = legacy.Username, DeviceId = device.DeviceId, DeviceToken = device.Token,
            InstallId = lastInstallId, PairedAt = device.PairedAt ?? now.ToString("O"), RotateAfter = device.RotateAfter, Extra = legacy.Extra,
        };
        try { SecureStore.Save(paired, path); }
        catch { SecureStore.DeleteRollback(path); if (run == siRun) CloseSignIn(); return; }
        view.SetStep(2);
        // 4. One request with the new key.
        var keyed = new DashboardClient(paired);
        try
        {
            var me = await keyed.DeviceMe();
            paired.RotateAfter = me.RotateAfter ?? paired.RotateAfter;
            SecureStore.DeleteRollback(path);
            view.SetStep(3);
            await Task.Delay(StepPace);
            view.SetStep(4);
            AdoptClient(paired, keyed);
            if (run == siRun) await SiSucceed(run, "Secured · this tray now signs in with a device key");
        }
        catch (DeviceSignedOutException)
        {
            keyed.Dispose();
            SecureStore.RestoreRollback(path);
            if (run == siRun) CloseSignIn();
        }
        catch (Exception)
        {
            // A network error: keep both files; the paired client checks again on the next poll.
            AdoptClient(paired, keyed);
            if (run == siRun) CloseSignIn();
        }
    }

    /// <summary>A pending migration (a rollback file next to a version 2 store): on the next good poll the key is
    /// checked once more. 200 deletes the rollback; a rollback older than 24 hours is deleted anyway.</summary>
    private async Task SettleRollback()
    {
        var path = ConnectionStorePath;
        if (client is not { Paired: true } paired || SecureStore.RollbackAge(path) is not TimeSpan age) return;
        if (age > TimeSpan.FromHours(24)) { try { SecureStore.DeleteRollback(path); } catch { } return; }
        var generation = connectionGeneration;
        try { await paired.DeviceMe(); if (ReferenceEquals(client, paired) && generation == connectionGeneration) SecureStore.DeleteRollback(path); }
        catch (DeviceSignedOutException) when (ReferenceEquals(client, paired) && generation == connectionGeneration) { throw; }
        catch { /* still offline (keep both and check again on the next poll), or the connection was replaced */ }
    }

    // ------------------------------------------------------------------ 7 · rotation

    /// <summary>After a good poll: refresh rotateAfter from devices/me every 6 hours, and rotate once it has passed
    /// (at most once an hour). The new key is written atomically before its first use; a deferred rotation (this
    /// connection is not trusted right now) keeps the current key and tries again later.</summary>
    internal async Task MaintainDeviceKey(bool force = false)
    {
        if (client is not { Paired: true } paired || connection is not { IsPaired: true } current) return;
        // A Re-pair, a Disconnect or a sign-out that lands while a request below is out replaces the connection: the
        // file it left is never overwritten with this older device's key, and whatever then happens to that device's
        // request (a 401, or the request cut off when its client is disposed) is not news.
        var generation = connectionGeneration;
        bool Current() => ReferenceEquals(client, paired) && generation == connectionGeneration;
        try { await MaintainKey(paired, current, Current, force); }
        catch (Exception) when (!Current()) { }
    }

    private async Task MaintainKey(DashboardClient paired, ConnectionSettings current, Func<bool> stillCurrent, bool force)
    {
        var now = DateTimeOffset.UtcNow;
        if (force || now - lastDeviceRead > TimeSpan.FromHours(6))
        {
            lastDeviceRead = now;
            var me = await paired.DeviceMe();
            if (!stillCurrent()) return;
            if (me.RotateAfter is { } after && after != current.RotateAfter)
            {
                current.RotateAfter = after;
                try { SecureStore.Save(current, ConnectionStorePath); } catch { }
            }
        }
        var due = DateTimeOffset.TryParse(current.RotateAfter, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var rotateAfter) && rotateAfter <= now;
        if (!force && (!due || now - lastRotateAttempt < TimeSpan.FromHours(1))) return;
        lastRotateAttempt = now;
        var rotated = await paired.Rotate();
        if (rotated is null || !stillCurrent()) return;
        var next = new ConnectionSettings
        {
            Version = 2, BaseURL = current.BaseURL, Username = current.Username, DeviceId = current.DeviceId, DeviceToken = rotated.Token,
            InstallId = current.InstallId, PairedAt = current.PairedAt, RotateAfter = rotated.RotateAfter, Extra = current.Extra,
        };
        // Saved before the first use: a crash here leaves the old key, which stays valid until the new one is used.
        SecureStore.Save(next, ConnectionStorePath);
        paired.UseToken(rotated.Token);
        connection = next;
    }

    // ------------------------------------------------------------------ 10 · signed out remotely

    /// <summary>
    /// A 401 device code (section 9): the key is deleted (the address and username stay), polling stops and the
    /// signed-out screen shows why. Never retried. During a migration whose key has not worked yet, version 1 comes
    /// back from the rollback instead.
    /// </summary>
    private void SignedOutRemotely(DeviceSignedOutException signedOut)
    {
        var path = ConnectionStorePath;
        if (SecureStore.RollbackAge(path) is TimeSpan age && age <= TimeSpan.FromHours(24))
        {
            try
            {
                SecureStore.RestoreRollback(path);
                var restored = SecureStore.Load(path);
                if (restored is { HasPassword: true })
                {
                    var previous = client;
                    connection = restored; client = new DashboardClient(restored);
                    connectionGeneration++; dashboard = null; openProgress.Clear();
                    previous?.Dispose();
                    FlashStatus("This tray went back to its saved password sign-in.");
                    return;
                }
            }
            catch { }
        }
        try { SecureStore.DeleteRollback(path); } catch { }
        var all = signedOut.RevokedReason is "revoke-all" or "revoke_all" or "revoke-all-devices";
        var reason = signedOut.Code == "device_revoked" && all ? "revoke_all" : signedOut.Code;
        var when = DateTimeOffset.TryParse(signedOut.RevokedAt, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var revokedAt) ? revokedAt : DateTimeOffset.UtcNow;
        var stub = new ConnectionSettings
        {
            Version = 2, BaseURL = connection?.BaseURL ?? "", Username = connection?.Username ?? "", InstallId = connection?.InstallId,
            SignedOutReason = reason, SignedOutAt = when.ToString("O"), RevokedBy = signedOut.RevokedBy,
        };
        try { if (stub.BaseURL.Length > 0) SecureStore.Save(stub, path); } catch { }
        if (DateTimeOffset.TryParse(dashboard?.UpdatedAt, CultureInfo.InvariantCulture, DateTimeStyles.RoundtripKind, out var updated)) lastGoodSample = updated;
        var gone = client;
        client = null; connection = stub; dashboard = null; connectionGeneration++;
        staleSample = false; statusFlash = null; openProgress.Clear();
        gone?.Dispose();
        foreach (var meter in meters.Values) Ui.Detach(meter);
        ContentPanel.Children.Clear();
        if (settingsVisible) CloseSettings(animate: false);
        ShowSignIn(StateFor(stub));
    }

    private static SignInState StateFor(ConnectionSettings signedOut) => signedOut.SignedOutReason switch
    {
        "device_expired" => SignInState.Expired,
        "revoke_all" => SignInState.SignedOutAll,
        "disconnected" => SignInState.FirstRun,
        _ => SignInState.SignedOut,
    };

    /// <summary>Every failure path passes here first: a sign-out is handled, and true is returned.</summary>
    private bool HandledSignOut(Exception error)
    {
        if (error is not DeviceSignedOutException { IsDeviceCode: true } signedOut) return false;
        SignedOutRemotely(signedOut);
        return true;
    }

    // ------------------------------------------------------------------ renders and checks: a state shown directly

    /// <summary>Render checks only: shows a state directly with example values (the concept's siPreset). Example
    /// addresses use the documentation-safe 192.168.1.x and example.net names. A variant shows a state's other forms:
    /// "upgrade" (a version 1 tray pairing from Settings), "legacy" (the password sign-in), "fallback" (pairing off,
    /// with the password option) and "uncertain" (a Re-pair whose pair request got no answer).</summary>
    internal void PresetSignInForCheck(SignInState state, bool repair = false, string? variant = null)
    {
        const string lan = "http://192.168.1.20:3000";
        repair |= variant == "uncertain";
        if (repair) { connection = new ConnectionSettings { Version = 2, BaseURL = lan, Username = "example", DeviceToken = "aacd_" + new string('A', 43), DeviceId = "dev_0000000000000000" }; client ??= new DashboardClient(connection); }
        else if (variant == "upgrade") { connection = new ConnectionSettings { BaseURL = lan, Username = "example", Password = "example-only" }; client ??= new DashboardClient(connection); }
        else if (state is SignInState.SignedOut or SignInState.SignedOutAll or SignInState.Expired)
            connection = new ConnectionSettings { Version = 2, BaseURL = lan, Username = "example", SignedOutReason = state == SignInState.Expired ? "device_expired" : state == SignInState.SignedOutAll ? "revoke_all" : "device_revoked", SignedOutAt = Formatting.Now().AddMinutes(-34).ToString("O") };
        else connection = null;
        lastGoodSample = state is SignInState.SignedOut or SignInState.SignedOutAll ? Formatting.Now().AddMinutes(-34) : null;
        ShowSignIn(state == SignInState.Success ? SignInState.Pairing : state, repair || variant == "upgrade", entrance: false);
        var view = SignIn;
        var model = view.Model;
        model.Verified = lan;
        model.LocalAddress = repair || state == SignInState.NotLocal ? lan : model.LocalAddress;
        model.Username = "example";
        if (state == SignInState.NotLocal) { model.Tried = "http://home.example.net:3000"; view.Addr.Value = model.Tried; }
        if (state == SignInState.Unreachable) { model.Tried = "http://192.168.1.24:3000"; view.Addr.Value = model.Tried; }
        if (state == SignInState.WrongAddress) { model.Tried = "http://192.168.1.20:8080"; view.Addr.Value = model.Tried; }
        if (state is SignInState.WrongPassword or SignInState.RateLimited or SignInState.Pairing or SignInState.Success or SignInState.SignedOut or SignInState.SignedOutAll or SignInState.Expired) view.User.Value = "example";
        if (state == SignInState.WrongPassword) { view.Pass.Value = "wrong-example-passphrase"; model.TriesLeft = 4; }
        if (state == SignInState.SetupCode) { view.User.Value = "example"; view.Pass.Value = "summit-ledger-4242"; view.Confirm.Value = "summit-ledger-4242"; }
        if (state == SignInState.RateLimited) { model.LimitTotal = TimeSpan.FromMinutes(15); model.LimitUntil = DateTimeOffset.UtcNow + TimeSpan.FromMinutes(15) - TimeSpan.FromSeconds(23); model.TriesLeft = 0; }
        model.Flow = state == SignInState.Securing ? SignInFlow.Secure : SignInFlow.Pair;
        model.Step = state is SignInState.Pairing or SignInState.Securing ? 2 : 4;
        model.Legacy = siLegacy = variant == "legacy";
        model.PasswordFallback = variant == "fallback";
        model.KeyUncertain = variant == "uncertain";
        view.Apply(state, instant: true);
        RenderFooter(); UpdateStatus();
    }

    // ------------------------------------------------------------------ Disconnect

    /// <summary>Settings › Disconnect: DELETE /api/auth/devices/me, then the key is deleted here and the first-run
    /// screen shows "Disconnected at ..." with the last address filled in. When the dashboard cannot be reached, nothing
    /// changes and the tray stays paired. Returns the message to show, or null.</summary>
    internal async Task<string?> DisconnectTray()
    {
        if (client is not { Paired: true } paired || connection is not { } current) return "This tray is not paired.";
        if (pairHeld) return "Pairing is finishing. Try again in a moment.";
        bool revoked;
        try { revoked = await paired.Disconnect(); }
        catch (Exception) { return "Can't reach the dashboard, so this tray is still paired. Try again."; }
        if (!revoked) return "The dashboard did not confirm the sign-out, so this tray is still paired. Try again.";
        var stub = new ConnectionSettings { Version = 2, BaseURL = current.BaseURL, Username = current.Username, InstallId = current.InstallId, SignedOutReason = "disconnected", SignedOutAt = DateTimeOffset.UtcNow.ToString("O") };
        try { SecureStore.Save(stub, ConnectionStorePath); SecureStore.DeleteRollback(ConnectionStorePath); }
        catch { return "Windows could not forget the device key. Try again."; }
        client = null; connection = stub; dashboard = null; connectionGeneration++; openProgress.Clear();
        paired.Dispose();
        foreach (var meter in meters.Values) Ui.Detach(meter);
        ContentPanel.Children.Clear();
        if (settingsVisible) CloseSettings(animate: false);
        ShowSignIn(SignInState.FirstRun);
        return null;
    }
}
