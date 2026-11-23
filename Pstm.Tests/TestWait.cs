namespace Pstm.Tests;

public static class TestWait
{
    public static bool Until(Func<bool> condition, TimeSpan? timeout = null)
    {
        DateTime deadline = DateTime.UtcNow + (timeout ?? TimeSpan.FromSeconds(5));
        while (DateTime.UtcNow < deadline)
        {
            if (condition())
            {
                return true;
            }

            Thread.Sleep(1);
        }

        if (condition())
        {
            return true;
        }

        throw new TimeoutException("the condition did not become true before the timeout");
    }
}
