# No GPU priority boost

The app does not raise its GPU scheduling priority. It sets no GPU thread priority on the capture device, no GPU scheduling priority class on the process, and no high priority on the compute queue. A boost applies to the whole process, not only to the inference, and it harms how the whole process runs. The CPU scheduling hints for the engine thread stay.

Considered options:

- Raise the GPU priority so that the inference is not queued behind the rendering of a game that uses the whole GPU. Rejected: the harm to the whole process is not worth a gain that was never measured.
- Add a setting for the boost. Rejected: for the same reason.
