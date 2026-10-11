# Context

The language of nice-aimassist. Use these terms in code, docs, and tickets. Do not invent synonyms. The rules for names are in [GLOSSARY-FORMAT.md](./.claude/skills/domain-modeling/GLOSSARY-FORMAT.md).

## Run

- **Run**: one period in which the engine processes frames, from a start to a stop.
- **Continuous run**: a Run that processes frames until it is stopped.
- **Run once**: a Run that processes one New frame and then stops.
- **Run generation**: the number of one Run. A result that carries an older number is discarded.
- **Shortcut**: a keyboard key combination or an XInput control that the user binds to an action.
- **Hold shortcut**: a Shortcut that keeps a Continuous run active while it is held down.

## Frame

- **Frame**: one image of the Model input region, with the time data of its capture.
- **Model input region**: the rectangle at the center of the captured display that has the size of the model input.
- **Present time**: the time of the desktop update that made a Frame.
- **New frame**: a Frame that comes from a new desktop update. Every Frame is a New frame.

## Pipeline

- **Pipeline**: the capture, the GPU preprocess, the inference session, and the model for one display. It is built and replaced as a unit.
- **Capture adapter**: the GPU that the captured display is attached to.
- **Candidate pipeline**: a Pipeline that is being prepared and is not yet in use.
- **Cold run**: the first inference after a model load. It is slower than the ones that follow.
- **Low-latency mode**: a mode in which the inference waits for the GPU without sleeping.

## Target

- **Detection**: one box that the model reports, with a confidence and a class.
- **Candidate detection**: a Detection whose confidence is at or above the Confidence threshold.
- **Confidence threshold**: the lowest confidence at which a Detection is accepted.
- **Selected target**: the Candidate detection nearest to the center of the Model input region.
- **Target track**: the Selected target followed across consecutive frames. Two boxes that overlap enough are the same target.
- **Target switch**: the first frame of a Target track.
- **Aim point**: the point inside the box of the Selected target that the output moves toward.
- **Target Y offset**: the height of the Aim point inside the box, from the top edge to the bottom edge.

## Output

- **MAK**: the hardware device that sends the mouse movement to the computer.
- **Mouse route**: the ability of the connected MAK to send mouse movement.
- **Injection API**: the command set that the app uses to talk to MAK.
- **Movement**: the mouse movement that the engine sends for one frame, in whole counts.
- **Sensitivity**: the gain from the distance to the Aim point to the Movement.
- **Max delta**: the longest Movement that one frame can send.
- **Rounding remainder**: the part of a Movement that is lost when it is rounded to whole counts. It is added to the next frame.

## Diagnostics

- **Frame trace**: a file that records every processed frame of one Run.
- **Loop metrics**: measurements of how well the output follows the target, such as the error and the time to reach the target.
