# GPU frame protocol

The app uses two Direct3D devices. The capture path and the model path share one image through two fences.
Read this document before you change `Capture/DesktopCapture.cs`, `Detect/Pipeline/Preprocessor.cs`,
`Detect/Pipeline/OutputReader.cs`, or `Detect/Runtime/GpuTimer.cs`.

## Devices

| Name | Object | Work |
| --- | --- | --- |
| capture device | `ID3D11Device` | DXGI output duplication |
| capture context | `ID3D11DeviceContext` (immediate, as `ID3D11DeviceContext4`) | copy into the shared texture, signal and wait on fences |
| compute device | `ID3D12Device` | preprocess, model, output copy |
| compute queue | `ID3D12CommandQueue` (compute, high priority) | every command list in this app |

The compute queue runs the preprocess, the model, and the output copy in the order that they are submitted.
A later list on the same queue sees the work of the earlier list. This ordering replaces a fence between the
three steps.

## Shared resources

| Resource | Creator | Other side |
| --- | --- | --- |
| shared texture, `B8G8R8A8_UNorm`, one mip, `SharedNthandle` | `DesktopCapture` on the capture device | opened as `ID3D12Resource` for the preprocess shader |
| capture copy done fence, value = frame sequence | `DesktopCapture` on the capture device (`ID3D11Fence`, shared) | opened on the compute device (`_copyDoneFence12`) |
| compute done fence, value = frame sequence | `DesktopCapture` on the compute device (`ID3D12Fence`, shared) | opened on the capture device (`_computeDoneFence11`) |
| output copy fence | `OutputReader` on the compute device | waited on by the loop thread |
| timestamp heap and its readback buffer | `GpuTimer` | written by the preprocess list and the output copy list |

The two frame fences carry the same `ulong` value. `CapturedFrame.Sequence` holds it.
`DesktopCapture` increments the value before every copy, so the value is never 0.

## One frame

1. The loop calls `TryAcquire`. DXGI gives the desktop texture of a new frame. If the previous frame is still
   held, `ReleaseFrame` returns it first.
2. The capture context copies the centered box of the desktop texture into the shared texture
   (`CopySubresourceRegion`).
3. `DesktopCapture` increments the sequence, signals the capture copy done fence with the new value, and
   calls `Flush`. Without the flush, the copy and the signal can stay in the driver and the compute queue can
   wait for work that the GPU does not see yet.
4. The loop calls `Preprocessor.Submit`:
   1. `BeginRead` makes the compute queue wait on the capture copy done fence.
   2. The preprocess list runs. It reads the shared texture and writes the model input tensor as planar RGB.
   3. `EndRead` makes the compute queue signal the compute done fence, then records the value as submitted.
5. `OnnxSession.Run` submits the model work to the compute queue and does not wait.
6. `OutputReader.Read` submits the output copy list, signals its own fence, and spins until the fence reaches
   the value. The span that it returns stays valid until the next `Read`.
7. The loop decodes the span on the CPU.
8. Before the next copy, the capture context waits on the compute done fence while the preprocess still reads
   the shared texture. `TryAcquire` skips this wait when the fence already passed.

## Why the two sides wait on each other

- The compute side waits for the capture copy, because the copy fills the texture that the preprocess reads.
- The capture side waits for the compute work, because the next copy overwrites the same texture.

A frame can be read one time, and only while it is the newest frame. `BeginRead` throws on any other frame.

## The dataset path

The dataset does not use these fences. It copies the desktop texture of the held frame into its own staging
texture on the capture context, flushes, and maps with `DoNotWait`. A map that reports that the GPU still
draws the copy returns false, and the loop tries again on a later pass. `StagingGate` prevents a second copy
while a copy is pending or mapped.

The reason is that the source and the destination both belong to the capture context. One context runs one
copy at a time, so the map only needs to wait for the GPU. The map does not wait, because the loop must not
block.

## Rules

- Only the loop thread calls the capture, the preprocess, the output read, and the dataset staging.
- `BeginRead` runs before `ExecuteCommandLists`. `EndRead` runs after it.
- The GPU timer starts at the first command of the preprocess list and ends at the first command of the output
  copy list. A change to either list moves the measured span.
- Dispose order: stop the loop, then the detector (preprocessor, output reader, timer), then the model session,
  then the capture, then the dataset recorder, then `GpuDevices`. Each owner waits for the work that can still
  use its resources. `GpuDevices.WaitIdle` drains the compute queue. `Preprocessor.Dispose` waits for the
  capture reads that it announced.
