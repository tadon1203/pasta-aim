using System.Collections.Concurrent;
using System.Text;

namespace Pstm.Shared.Logging;

/// <summary>Writes lines to a file on its own thread. <see cref="Write"/> never blocks. A full queue drops the line.</summary>
internal sealed class LogWriter : IDisposable
{
    private const int MaxPending = 65_536;

    private readonly BlockingCollection<string> _lines = new(MaxPending);
    private readonly StreamWriter _writer;
    private readonly Thread _thread;

    public LogWriter(string path)
    {
        _writer = new StreamWriter(
            new FileStream(path, FileMode.Create, FileAccess.Write, FileShare.Read), new UTF8Encoding(false));
        _thread = new Thread(Run) { Name = "log", IsBackground = true };
        _thread.Start();
    }

    public void Write(string line)
    {
        try
        {
            _lines.TryAdd(line);
        }
        catch (InvalidOperationException)
        {
        }
    }

    public void Dispose()
    {
        _lines.CompleteAdding();
        _thread.Join();
        _writer.Dispose();
        _lines.Dispose();
    }

    private void Run()
    {
        try
        {
            foreach (string line in _lines.GetConsumingEnumerable())
            {
                _writer.WriteLine(line);
                if (_lines.Count == 0)
                {
                    _writer.Flush();
                }
            }

            _writer.Flush();
        }
        catch (Exception error) when (error is IOException or ObjectDisposedException)
        {
        }
    }
}
