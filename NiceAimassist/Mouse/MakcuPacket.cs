namespace NiceAimassist.Mouse;

internal readonly record struct MakcuPacket(byte Command, byte[] Payload);
