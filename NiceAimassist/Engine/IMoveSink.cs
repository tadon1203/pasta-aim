using NiceAimassist.Config;

namespace NiceAimassist.Engine;

/// <summary>The mouse output of the engine. Only the engine thread calls it.</summary>
public interface IMoveSink
{
    void Move(short dx, short dy);

    void SetInterpolation(Interpolation interpolation);
}
