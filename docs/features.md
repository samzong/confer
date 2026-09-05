# Feature explorations

These notes capture future exploration without committing work to the roadmap or blocking ongoing iteration. The supported product contract remains in [SPEC.md](SPEC.md).

## Queue, steering, and progress visibility

The goal is to let a host observe a seat's work and intervene when useful. Confer currently provides an in-process FIFO queue and final-answer retrieval.

- Keep the current queue. Removing it has no demonstrated benefit today. Native queues introduce protocol and version differences; returning `busy` instead transfers pending-message management and retry calls to the host. Neither approach removes the need for execution state, concurrency control, and result tracking.
- Treat steering as an independent capability that can coexist with the queue. A future adapter must establish support through its actual native interface. Unsupported steering should be reported explicitly; cancellation followed by another prompt is a separate action.
- Explore bounded progress and tool-event summaries first. The host must receive useful events through a model-visible path to act before completion. Sending every reasoning fragment or full tool result by default would increase context and processing costs. Steering alone does not provide this visibility.

Revisit when real tasks show that waiting for final answers causes avoidable work or late corrections. Start with one frequently used agent and verify whether intermediate observations change the host's decisions enough to justify the integration and context costs.

Record follow-up findings here with the tested interface, version, and evidence. Implementation scope and product semantics require a separate decision.
