using System.Runtime.Versioning;
using System.Security.Principal;

namespace CCS.QwenUsageBridge;

internal sealed class CollectionLease : IDisposable
{
    private readonly Mutex mutex;
    private bool released;
    private CollectionLease(Mutex mutex) => this.mutex = mutex;

    [SupportedOSPlatform("windows")]
    internal static CollectionLease Acquire()
    {
        using var identity = WindowsIdentity.GetCurrent();
        var sid = identity.User?.Value ?? throw new SafeFailure("unavailable");
        // Global namespace covers another browser/session for this SAME user;
        // separate users have separate profile caches and separate lock names.
        var mutex = new Mutex(false, @"Global\CCS.QwenUsageBridge." + sid);
        try
        {
            var acquired = false;
            try { acquired = mutex.WaitOne(TimeSpan.FromSeconds(5)); }
            catch (AbandonedMutexException) { acquired = true; }
            if (!acquired)
                throw new SafeFailure("busy");
            return new CollectionLease(mutex);
        }
        catch
        {
            mutex.Dispose();
            throw;
        }
    }

    public void Dispose()
    {
        if (released)
            return;
        released = true;
        try { mutex.ReleaseMutex(); }
        finally { mutex.Dispose(); }
    }
}
