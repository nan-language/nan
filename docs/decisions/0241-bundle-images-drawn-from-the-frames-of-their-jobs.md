# 0241. Bundle images drawn from the frames of their jobs

Status: accepted

## Context

An isolated space drew the image of each bundle it mapped from the frames `isolate` borrowed, and kept it until it ran nothing ([0222](0222-isolated-spaces-shared-by-jobs-and-instances.md), [0238](0238-frames-each-job-and-instance-lends-its-space.md)). NaN OS keeps a space per principal and provenance class for as long as it runs anything, starts and stops instances of many bundles there, charges a loaded bundle to the instance whose start loaded it, passes the charge to another instance using it when that one stops, and unmaps the bundle once none uses it. A kernel had to lend at `isolate` the images of every bundle the space would ever map: lent too few, a later image took frames running jobs lent, and once one of them ended every job of the space ended exhausted; lent enough, unused images held frames and window room. The runtime walks the line tables of every image it maps whenever it computes a stack's reserve or locates a code address, and its thread can be interrupted inside such a walk.

## Decision

- **A job's start maps its bundle.** The isolation joins a job's or instance's bundle when the runtime takes its start, drawing the image from the isolation's frames against the frames the job lent; its budget is what it lent beyond them, and it is unplaced when it lent fewer. A job never started never maps its bundle.
- **The last job of a bundle unmaps it.** `sys::load::Isolated.join` counts the joins of each image, and `Isolated.leave` undoes one, unlinking the image's line tables and unmapping it, and its tables of pages left empty, with the last. A started job ends only at the runtime's `END` call or with every job of its space, so its image is never unmapped while the thread may be walking it.
- **Charges follow the frames.** A job gets back what it lent, less what its start drew and plus what leaving gave back as it ended, so the frames of an image return with the last job using it, whoever mapped it.
- **A space maps no bundle when it is made.** `Isolated.space` maps the runtime alone; `join` places each image at the first room after the boot area that holds it.

Alternatives rejected:

- **An `Isolation` operation lending frames to the space itself and one taking back those of unmapped images:** the kernel would size images it cannot see and match loans to bundles itself, and the space would still unmap images by the same rule.
- **Mapping a job's bundle as it is placed:** a job abandoned before it starts would unmap an image while the thread may be inside a walk of its line tables.
- **Passing an image's charge to a named job using it:** the isolation would have to tell which jobs share an image, while counting joins in the space and giving the frames to whichever job ends last does the same with no such knowledge.

## Consequences

- A job or instance lends the frames of its bundle's image whenever it may be the one mapping it, and one ending while another of its bundle runs gets back fewer frames than it lent, the last one more.
- A job whose image does not fit its loan or the window is unplaced only as the runtime would start it.
- A table of pages that another image shares stays with the space once the image that took it is left, so a job can get back a frame less, which the job leaving the table empty gets back.
- Supersedes 0222's consequence that the images a space maps grow until it ends and its decision that a space maps bundles one after another after its boot area, and 0238's decision that the frames of `isolate` serve the images of the space's bundles.
