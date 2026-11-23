namespace Pstm.Mouse;

/// <summary>The mouse output of the loop. Only the loop thread calls it.</summary>
public interface IMouse
{
    void Move(short dx, short dy);

    void SetInterpolation(Interpolation interpolation);
}
