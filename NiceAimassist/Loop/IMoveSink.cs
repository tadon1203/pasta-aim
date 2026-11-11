using NiceAimassist.Config;

namespace NiceAimassist.Loop;

/// <summary>The mouse output of the loop. Only the loop thread calls it.</summary>
public interface IMoveSink
{
    void Move(short dx, short dy);

    void SetInterpolation(Interpolation interpolation);
}
