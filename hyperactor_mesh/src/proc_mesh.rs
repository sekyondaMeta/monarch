/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::panic::Location;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use dashmap::DashMap;
use futures::future::join_all;
use hyperactor::Actor;
use hyperactor::ActorHandle;
use hyperactor::ActorId;
use hyperactor::ActorRef;
use hyperactor::Instance;
use hyperactor::Mailbox;
use hyperactor::Named;
use hyperactor::RemoteMessage;
use hyperactor::WorldId;
use hyperactor::actor::ActorStatus;
use hyperactor::actor::RemoteActor;
use hyperactor::actor::remote::Remote;
use hyperactor::cap;
use hyperactor::channel;
use hyperactor::channel::ChannelAddr;
use hyperactor::mailbox;
use hyperactor::mailbox::BoxableMailboxSender;
use hyperactor::mailbox::BoxedMailboxSender;
use hyperactor::mailbox::DialMailboxRouter;
use hyperactor::mailbox::MailboxServer;
use hyperactor::mailbox::MessageEnvelope;
use hyperactor::mailbox::PortHandle;
use hyperactor::mailbox::PortReceiver;
use hyperactor::mailbox::Undeliverable;
use hyperactor::metrics;
use hyperactor::proc::Proc;
use hyperactor::reference::ProcId;
use hyperactor::supervision::ActorSupervisionEvent;
use ndslice::Range;
use ndslice::Shape;
use ndslice::ShapeError;
use strum::AsRefStr;
use tokio::sync::mpsc;
use tracing::Instrument;
use tracing::Level;
use tracing::span;

use crate::CommActor;
use crate::Mesh;
use crate::actor_mesh::CAST_ACTOR_MESH_ID;
use crate::actor_mesh::RootActorMesh;
use crate::alloc::Alloc;
use crate::alloc::AllocExt;
use crate::alloc::AllocatedProc;
use crate::alloc::AllocatorError;
use crate::alloc::ProcState;
use crate::alloc::ProcStopReason;
use crate::assign::Ranks;
use crate::comm::CommActorMode;
use crate::proc_mesh::mesh_agent::GspawnResult;
use crate::proc_mesh::mesh_agent::MeshAgentMessageClient;
use crate::proc_mesh::mesh_agent::ProcMeshAgent;
use crate::proc_mesh::mesh_agent::StopActorResult;
use crate::reference::ProcMeshId;
use crate::router;
use crate::shortuuid::ShortUuid;

pub mod mesh_agent;

use std::sync::OnceLock;
use std::sync::RwLock;

/// Single, process-wide supervision sink storage.
///
/// This is a pragmatic "good enough for now" global used to route
/// undeliverables observed by the process-global root client (c.f.
/// [`global_root_client`])to the *currently active* `ProcMesh`. Newer
/// meshes override older ones ("last sink wins").
static GLOBAL_SUPERVISION_SINK: OnceLock<RwLock<Option<PortHandle<ActorSupervisionEvent>>>> =
    OnceLock::new();

/// Returns the lazily-initialized container that holds the current
/// process-global supervision sink.
///
/// Internal helper: callers should use `set_global_supervision_sink`
/// and `get_global_supervision_sink` instead.
fn sink_cell() -> &'static RwLock<Option<PortHandle<ActorSupervisionEvent>>> {
    GLOBAL_SUPERVISION_SINK.get_or_init(|| RwLock::new(None))
}

/// Install (or replace) the process-global supervision sink.
///
/// This function enforces "last sink wins" semantics: if a sink was
/// already installed, it is replaced and the previous sink is
/// returned. Called from `ProcMesh::allocate_boxed`, after creating
/// the mesh's supervision port.
///
/// Returns:
/// - `Some(prev)` if a prior sink was installed, allowing the caller
///   to log/inspect it if desired;
/// - `None` if this is the first sink.
///
/// Thread-safety: takes a write lock briefly to swap the handle.
pub(crate) fn set_global_supervision_sink(
    sink: PortHandle<ActorSupervisionEvent>,
) -> Option<PortHandle<ActorSupervisionEvent>> {
    let cell = sink_cell();
    let mut guard = cell.write().unwrap();
    let prev = guard.take();
    *guard = Some(sink);
    prev
}

/// Get a clone of the current process-global supervision sink, if
/// any.
///
/// This is used by the process-global root client [c.f.
/// `global_root_client`] to forward undeliverables once a mesh has
/// installed its sink. If no sink has been installed yet, returns
/// `None` and callers should defer/ignore forwarding until one
/// appears.
///
/// Thread-safety: takes a read lock briefly; cloning the `PortHandle`
/// is cheap.
pub(crate) fn get_global_supervision_sink() -> Option<PortHandle<ActorSupervisionEvent>> {
    sink_cell().read().unwrap().clone()
}

/// Context use by root client to send messages.
/// This mailbox allows us to open ports before we know which proc the
/// messages will be sent to.
pub fn global_root_client() -> &'static Instance<()> {
    static GLOBAL_INSTANCE: OnceLock<(Instance<()>, ActorHandle<()>)> = OnceLock::new();
    let (instance, _) = GLOBAL_INSTANCE.get_or_init(|| {
        let world_id = WorldId(ShortUuid::generate().to_string());
        let client_proc_id = ProcId::Ranked(world_id.clone(), 0);
        let client_proc = Proc::new(client_proc_id.clone(), router::global().boxed());
        router::global().bind(world_id.clone().into(), client_proc.clone());

        let (client, handle) = client_proc
            .instance("client")
            .expect("root instance create");

        // Bind the global root client's undeliverable port and
        // forward any undeliverable messages to the currently active
        // supervision sink.
        //
        // The resolver (`get_global_supervision_sink`) is passed as a
        // function pointer, so each time an undeliverable is
        // processed, we look up the *latest* sink. This allows the
        // root client to seamlessly track whichever ProcMesh most
        // recently installed a supervision sink (e.g., the
        // application mesh instead of an internal controller mesh).
        //
        // The hook logs each undeliverable, along with whether a sink
        // was present at the time of receipt, which helps diagnose
        // lost or misrouted events.
        let (undeliverable_tx, undeliverable_rx) =
            client.open_port::<Undeliverable<MessageEnvelope>>();
        undeliverable_tx.bind_to(Undeliverable::<MessageEnvelope>::port());
        hyperactor::mailbox::supervise_undeliverable_messages_with(
            undeliverable_rx,
            crate::proc_mesh::get_global_supervision_sink,
            |env| {
                let sink_present = crate::proc_mesh::get_global_supervision_sink().is_some();
                tracing::info!(
                    actor_id = %env.dest().actor_id(),
                    "global root client undeliverable observed with headers {:?} {}", env.headers(), sink_present
                );
            },
        );

        (client, handle)
    });
    instance
}

type ActorEventRouter = Arc<DashMap<ActorMeshName, mpsc::UnboundedSender<ActorSupervisionEvent>>>;
/// A ProcMesh maintains a mesh of procs whose lifecycles are managed by
/// an allocator.
pub struct ProcMesh {
    // The underlying set of events. It is None if it has been transferred to
    // a proc event observer.
    event_state: Option<EventState>,
    actor_event_router: ActorEventRouter,
    shape: Shape,
    ranks: Vec<(ShortUuid, ProcId, ChannelAddr, ActorRef<ProcMeshAgent>)>,
    #[allow(dead_code)] // will be used in subsequent diff
    client_proc: Proc,
    client: Mailbox,
    comm_actors: Vec<ActorRef<CommActor>>,
    world_id: WorldId,
}

struct EventState {
    alloc: Box<dyn Alloc + Send + Sync>,
    supervision_events: PortReceiver<ActorSupervisionEvent>,
}

impl ProcMesh {
    #[hyperactor::instrument(fields(name = "ProcMesh::allocate"))]
    pub async fn allocate(
        alloc: impl Alloc + Send + Sync + 'static,
    ) -> Result<Self, AllocatorError> {
        ProcMesh::allocate_boxed(Box::new(alloc)).await
    }

    /// Allocate a new ProcMesh from the provided allocator. Allocate returns
    /// after the mesh has been successfully (and fully) allocated, returning
    /// early on any allocation failure.
    #[track_caller]
    pub fn allocate_boxed(
        alloc: Box<dyn Alloc + Send + Sync>,
    ) -> impl std::future::Future<Output = Result<Self, AllocatorError>> {
        Self::allocate_boxed_inner(alloc, Location::caller())
    }

    fn alloc_counter() -> &'static AtomicUsize {
        static C: OnceLock<AtomicUsize> = OnceLock::new();
        C.get_or_init(|| AtomicUsize::new(0))
    }

    #[hyperactor::observe_result("ProcMesh")]
    async fn allocate_boxed_inner(
        mut alloc: Box<dyn Alloc + Send + Sync>,
        loc: &'static Location<'static>,
    ) -> Result<Self, AllocatorError> {
        let alloc_id = Self::alloc_counter().fetch_add(1, Ordering::Relaxed) + 1;
        let world = alloc.world_id().name().to_string();
        tracing::info!(
            name = "ProcMesh::Allocate::Attempt",
            %world,
            alloc_id,
            caller = %format!("{}:{}", loc.file(), loc.line()),
            shape = ?alloc.shape(),
            "allocating proc mesh"
        );

        // 1. Initialize the alloc, producing the initial set of ranked procs:
        let running = alloc
            .initialize()
            .instrument(span!(
                Level::INFO,
                "ProcMesh::Allocate::Initialize",
                alloc_id
            ))
            .await?;

        // 2. Set up routing to the initialized procs; these require dialing.
        // let router = DialMailboxRouter::new();
        let router = DialMailboxRouter::new_with_default(router::global().boxed());
        for AllocatedProc { proc_id, addr, .. } in running.iter() {
            if proc_id.is_direct() {
                continue;
            }
            router.bind(proc_id.clone().into(), addr.clone());
        }

        // 3. Set up a client proc for the mesh itself, so that we can attach ourselves
        //    to it, and communicate with the agents. We wire it into the same router as
        //    everything else, so now the whole mesh should be able to communicate.
        let client_proc_id =
            ProcId::Ranked(WorldId(format!("{}_client", alloc.world_id().name())), 0);
        let (client_proc_addr, client_rx) = channel::serve(ChannelAddr::any(alloc.transport()))
            .await
            .map_err(|err| AllocatorError::Other(err.into()))?;
        tracing::info!(
            name = "ProcMesh::Allocate::ChannelServe",
            alloc_id = alloc_id,
            "client proc started listening on addr: {client_proc_addr}"
        );
        let client_proc = Proc::new(
            client_proc_id.clone(),
            BoxedMailboxSender::new(router.clone()),
        );
        client_proc.clone().serve(client_rx);
        router.bind(client_proc_id.clone().into(), client_proc_addr.clone());

        // 4. Bind the dial router to the global router, so that everything is
        //    connected to a single root.
        router::global().bind_dial_router(&router);

        let supervisor = client_proc.attach("supervisor")?;
        let (supervision_port, supervision_events) =
            supervisor.open_port::<ActorSupervisionEvent>();

        // 5. Install this mesh’s supervision sink.
        //
        // We intentionally use "last sink wins": if multiple
        // ProcMeshes exist in the process (e.g., a hidden
        // controller_controller mesh and the app/test mesh), the most
        // recently allocated mesh’s sink replaces the prior global
        // sink.
        //
        // Scope: this only affects undeliverables that arrive on the
        // `global_root_client()` undeliverable port. Per-mesh client
        // bindings (set up below) are unaffected and continue to
        // forward their own undeliverables to this mesh’s
        // `supervision_port`.
        //
        // NOTE: This is a pragmatic stopgap to restore correct
        // routing with multiple meshes in-process. If/when we move to
        // per-world root clients, this override can be removed.
        let _prev = set_global_supervision_sink(supervision_port.clone());

        // Wire this mesh’s *own* client mailbox to supervision.
        //
        // Attach a client mailbox for this `ProcMesh`, bind its
        // undeliverable port, and forward those undeliverables as
        // `ActorSupervisionEvent` records into this mesh's
        // supervision_port.
        //
        // Scope: covers undeliverables observed on this mesh's client
        // mailbox only. It does not affect other meshes or the
        // `global_root_client()`.
        let client = client_proc.attach("client")?;
        // Bind an undeliverable message port in the client.
        let (undeliverable_messages, client_undeliverable_receiver) =
            client.open_port::<Undeliverable<MessageEnvelope>>();
        undeliverable_messages.bind_to(Undeliverable::<MessageEnvelope>::port());
        hyperactor::mailbox::supervise_undeliverable_messages(
            supervision_port.clone(),
            client_undeliverable_receiver,
            |env| {
                tracing::info!(actor=%env.dest().actor_id(), "per-mesh client undeliverable observed");
            },
        );

        // Ensure that the router is served so that agents may reach us.
        let (router_channel_addr, router_rx) = channel::serve(ChannelAddr::any(alloc.transport()))
            .await
            .map_err(|err| AllocatorError::Other(err.into()))?;
        router.serve(router_rx);
        tracing::info!("router channel started listening on addr: {router_channel_addr}");

        // 6. Configure the mesh agents. This transmits the address book to all agents,
        //    so that they can resolve and route traffic to all nodes in the mesh.
        let address_book: HashMap<_, _> = running
            .iter()
            .map(
                |AllocatedProc {
                     addr, mesh_agent, ..
                 }| { (mesh_agent.actor_id().proc_id().clone(), addr.clone()) },
            )
            .collect();

        let (config_handle, mut config_receiver) = client.open_port();
        for (rank, AllocatedProc { mesh_agent, .. }) in running.iter().enumerate() {
            mesh_agent
                .configure(
                    &client,
                    rank,
                    router_channel_addr.clone(),
                    Some(supervision_port.bind()),
                    address_book.clone(),
                    config_handle.bind(),
                )
                .await?;
        }
        let mut completed = Ranks::new(running.len());
        while !completed.is_full() {
            let rank = config_receiver
                .recv()
                .await
                .map_err(|err| AllocatorError::Other(err.into()))?;
            if completed.insert(rank, rank).is_some() {
                tracing::warn!("multiple completions received for rank {}", rank);
            }
        }

        // For reasons I fail to fully understand, the below call fails
        // when invoked from `pyo3_async_runtimes::tokio::future_into_py`
        // when using a closure. It appears to be some subtle failure of
        // the compiler to unify lifetimes. If we use a function instead,
        // it does better.
        //
        // Interestingly, this only appears to fail in *specific* caller
        // contexts (e.g., https://fburl.com/code/evfgtfx1), and the error
        // is reported there as "implementation of `std::ops::FnOnce` is not general enough",
        // suggesting some failure of modularity in the compiler's lifetime
        // unification!
        //
        // Baffling and unsettling.
        fn project_mesh_agent_ref(allocated_proc: &AllocatedProc) -> ActorRef<ProcMeshAgent> {
            allocated_proc.mesh_agent.clone()
        }

        // 7. Start comm actors and set them up to communicate via the same address book.

        // Spawn a comm actor on each proc, so that they can be used
        // to perform tree distribution and accumulation.
        let comm_actors = Self::spawn_on_procs::<CommActor>(
            &client,
            running.iter().map(project_mesh_agent_ref),
            "comm",
            &Default::default(),
        )
        .await?;
        let address_book: HashMap<_, _> = comm_actors.iter().cloned().enumerate().collect();
        // Now that we have all of the spawned comm actors, kick them all into
        // mesh mode.
        for (rank, comm_actor) in comm_actors.iter().enumerate() {
            comm_actor
                .send(&client, CommActorMode::Mesh(rank, address_book.clone()))
                .map_err(anyhow::Error::from)?;
        }

        let shape = alloc.shape().clone();
        let world_id = alloc.world_id().clone();
        metrics::PROC_MESH_ALLOCATION.add(1, hyperactor_telemetry::kv_pairs!());

        Ok(Self {
            event_state: Some(EventState {
                alloc,
                supervision_events,
            }),
            actor_event_router: Arc::new(DashMap::new()),
            shape,
            ranks: running
                .into_iter()
                .map(
                    |AllocatedProc {
                         create_key,
                         proc_id,
                         addr,
                         mesh_agent,
                     }| (create_key, proc_id, addr, mesh_agent),
                )
                .collect(),
            client_proc,
            client,
            comm_actors,
            world_id,
        })
    }

    async fn spawn_on_procs<A: Actor + RemoteActor>(
        cx: &(impl cap::CanSend + cap::CanOpenPort),
        agents: impl IntoIterator<Item = ActorRef<ProcMeshAgent>> + '_,
        actor_name: &str,
        params: &A::Params,
    ) -> Result<Vec<ActorRef<A>>, anyhow::Error>
    where
        A::Params: RemoteMessage,
    {
        let remote = Remote::collect();
        let actor_type = remote
            .name_of::<A>()
            .ok_or(anyhow::anyhow!("actor not registered"))?
            .to_string();

        let (completed_handle, mut completed_receiver) = mailbox::open_port(cx);
        let mut n = 0;
        for agent in agents {
            agent
                .gspawn(
                    cx,
                    actor_type.clone(),
                    actor_name.to_string(),
                    bincode::serialize(params)?,
                    completed_handle.bind(),
                )
                .await?;
            n += 1;
        }
        let mut completed = Ranks::new(n);
        while !completed.is_full() {
            let result = completed_receiver.recv().await?;
            match result {
                GspawnResult::Success { rank, actor_id } => {
                    if completed.insert(rank, actor_id).is_some() {
                        tracing::warn!("multiple completions received for rank {}", rank);
                    }
                }
                GspawnResult::Error(error_msg) => {
                    metrics::PROC_MESH_ACTOR_FAILURES.add(
                        1,
                        hyperactor_telemetry::kv_pairs!(
                            "actor_name" => actor_name.to_string(),
                            "error" => error_msg.clone(),
                        ),
                    );

                    anyhow::bail!("gspawn failed: {}", error_msg);
                }
            }
        }

        // `Ranks` really should have some way to convert into a "completed" rank
        // in a one-shot way; the API here is too awkward otherwise.
        Ok(completed
            .into_iter()
            .map(Option::unwrap)
            .map(ActorRef::attest)
            .collect())
    }

    fn agents(&self) -> impl Iterator<Item = ActorRef<ProcMeshAgent>> + '_ {
        self.ranks.iter().map(|(_, _, _, agent)| agent.clone())
    }

    /// Return the comm actor to which casts should be forwarded.
    pub(crate) fn comm_actor(&self) -> &ActorRef<CommActor> {
        &self.comm_actors[0]
    }

    /// Spawn an `ActorMesh` by launching the same actor type on all
    /// agents, using the **same** parameters instance for every
    /// actor.
    ///
    /// - `actor_name`: Name for all spawned actors.
    /// - `params`: Reference to the parameter struct, reused for all
    ///   actors.
    pub async fn spawn<A: Actor + RemoteActor>(
        &self,
        actor_name: &str,
        params: &A::Params,
    ) -> Result<RootActorMesh<'_, A>, anyhow::Error>
    where
        A::Params: RemoteMessage,
    {
        let (tx, rx) = mpsc::unbounded_channel::<ActorSupervisionEvent>();
        {
            // Instantiate supervision routing BEFORE spawning the actor mesh.
            self.actor_event_router.insert(actor_name.to_string(), tx);
            tracing::info!(
                name = "router_insert",
                actor_name = %actor_name,
                "the length of the router is {}", self.actor_event_router.len(),
            );
        }
        let root_mesh = RootActorMesh::new(
            self,
            actor_name.to_string(),
            rx,
            Self::spawn_on_procs::<A>(&self.client, self.agents(), actor_name, params).await?,
        );
        Ok(root_mesh)
    }

    /// A client used to communicate with any member of this mesh.
    pub fn client(&self) -> &Mailbox {
        &self.client
    }

    pub fn client_proc(&self) -> &Proc {
        &self.client_proc
    }

    pub fn proc_id(&self) -> &ProcId {
        self.client_proc.proc_id()
    }

    pub fn world_id(&self) -> &WorldId {
        &self.world_id
    }

    /// An event stream of proc events. Each ProcMesh can produce only one such
    /// stream, returning None after the first call.
    pub fn events(&mut self) -> Option<ProcEvents> {
        self.event_state.take().map(|event_state| ProcEvents {
            event_state,
            ranks: self
                .ranks
                .iter()
                .enumerate()
                .map(|(rank, (create_key, proc_id, _addr, _mesh_agent))| {
                    (proc_id.clone(), (rank, create_key.clone()))
                })
                .collect(),
            actor_event_router: self.actor_event_router.clone(),
        })
    }

    pub fn shape(&self) -> &Shape {
        &self.shape
    }

    /// Send stop actors message to all mesh agents for a specific mesh name
    #[hyperactor::observe_result("ProcMesh")]
    pub async fn stop_actor_by_name(&self, mesh_name: &str) -> Result<(), anyhow::Error> {
        let timeout = hyperactor::config::global::get(hyperactor::config::STOP_ACTOR_TIMEOUT);
        let results = join_all(self.agents().map(|agent| async move {
            let actor_id = ActorId(agent.actor_id().proc_id().clone(), mesh_name.to_string(), 0);
            (
                actor_id.clone(),
                agent
                    .clone()
                    .stop_actor(&self.client, actor_id, timeout.as_millis() as u64)
                    .await,
            )
        }))
        .await;

        for (actor_id, result) in results {
            match result {
                Ok(StopActorResult::Timeout) => {
                    tracing::warn!("timed out while stopping actor {}", actor_id);
                }
                Ok(StopActorResult::NotFound) => {
                    tracing::warn!("no actor {} on proc {}", actor_id, actor_id.proc_id());
                }
                Ok(StopActorResult::Success) => {
                    tracing::info!("stopped actor {}", actor_id);
                }
                Err(e) => {
                    tracing::warn!("error stopping actor {}: {}", actor_id, e);
                }
            }
        }
        Ok(())
    }
}

/// Proc lifecycle events.
#[derive(Debug, Clone)]
pub enum ProcEvent {
    /// The proc of the given rank was stopped with the provided reason.
    Stopped(usize, ProcStopReason),
    /// The proc crashed, with the provided "reason". This is reserved for
    /// unhandled supervision events.
    Crashed(usize, String),
}

#[derive(Debug, Clone, AsRefStr)]
pub enum SupervisionEventState {
    SupervisionEventForward,
    SupervisionEventForwardFailed,
    SupervisionEventReceived,
    SupervisionEventTransmitFailed,
}

impl fmt::Display for ProcEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProcEvent::Stopped(rank, reason) => {
                write!(f, "Proc at rank {} stopped: {}", rank, reason)
            }
            ProcEvent::Crashed(rank, reason) => {
                write!(f, "Proc at rank {} crashed: {}", rank, reason)
            }
        }
    }
}

type ActorMeshName = String;

/// An event stream of [`ProcEvent`]
// TODO: consider using streams for this.
pub struct ProcEvents {
    event_state: EventState,
    // Proc id to its rank and create key.
    ranks: HashMap<ProcId, (usize, ShortUuid)>,
    actor_event_router: ActorEventRouter,
}

impl ProcEvents {
    /// Get the next lifecycle event. The stream is closed when this method
    /// returns `None`.
    pub async fn next(&mut self) -> Option<ProcEvent> {
        loop {
            tokio::select! {
                result = self.event_state.alloc.next() => {
                    tracing::debug!(name = "ProcEventReceived", "received ProcEvent alloc update: {result:?}");
                    // Don't disable the outer branch on None: this is always terminal.
                    let Some(alloc_event) = result else {
                        self.actor_event_router.clear();
                        break None;
                    };

                    let ProcState::Stopped { create_key, reason } = alloc_event else {
                        // Ignore non-stopped events for now.
                        continue;
                    };

                    let Some((proc_id, (rank, _create_key))) = self.ranks.iter().find(|(_proc_id, (_, key))| key == &create_key) else {
                        tracing::warn!("received stop event for unmapped proc {}", create_key);
                        continue;
                    };

                    metrics::PROC_MESH_PROC_STOPPED.add(
                        1,
                        hyperactor_telemetry::kv_pairs!(
                            "create_key" => create_key.to_string(),
                            "rank" => rank.to_string(),
                            "reason" => reason.to_string(),
                        ),
                    );

                    // Need to send this event to actor meshes to notify them of the proc's death.
                    // TODO(albertli): only send this event to all root actor meshes if any of them use this proc.
                    for entry in self.actor_event_router.iter() {
                        // Make a dummy actor supervision event, all actors on the proc are affected if a proc stops.
                        // TODO(T231868026): find a better way to represent all actors in a proc for supervision event
                        let event = ActorSupervisionEvent {
                            actor_id: proc_id.actor_id("any", 0),
                            actor_status: ActorStatus::Failed(format!("proc {} is stopped", proc_id)),
                            message_headers: None,
                            caused_by: None,
                        };
                        if entry.value().send(event).is_err() {
                            tracing::warn!(
                                name = SupervisionEventState::SupervisionEventTransmitFailed.as_ref(),
                                "unable to transmit supervision event to actor {}", entry.key()
                            );
                        }
                    }

                    break Some(ProcEvent::Stopped(*rank, reason));
                }

                // Supervision events for this ProcMesh, delivered on
                // the client's "supervisor" port. Some failures are
                // observed while messages are routed through the
                // comm-actor tree; in those cases the event's
                // `actor_id` points at a comm actor rather than the
                // logical actor-mesh. When the `CAST_ACTOR_MESH_ID`
                // header is present, we normalize the event by
                // rewriting `actor_id` to a synthetic mesh-level id
                // so that routing reaches the correct `ActorMesh`
                // subscribers.
                Ok(mut event) = self.event_state.supervision_events.recv() => {
                    let had_headers = event.message_headers.is_some();
                    tracing::info!(
                        name = SupervisionEventState::SupervisionEventReceived.as_ref(),
                        actor_id = %event.actor_id,
                        actor_name = %event.actor_id.name(),
                        status = %event.actor_status,
                        "proc supervision: event received with {had_headers} headers"
                    );
                    tracing::debug!(?event, "proc supervision: full event");

                    // Normalize events that came via the comm tree.
                    if let Some(headers) = &event.message_headers {
                        if let Some(actor_mesh_id) = headers.get(CAST_ACTOR_MESH_ID) {
                            let old_actor = event.actor_id.clone();
                            event.actor_id = ActorId(
                                ProcId::Ranked(WorldId(actor_mesh_id.0.0.clone()), 0),
                                actor_mesh_id.1.clone(),
                                0,
                            );
                            tracing::debug!(
                                actor_id = %old_actor,
                                "proc supervision: remapped comm-actor id to mesh id from CAST_ACTOR_MESH_ID {}", event.actor_id
                            );
                        } else {
                            tracing::debug!(
                                "proc supervision: headers present but no CAST_ACTOR_MESH_ID; leaving actor_id unchanged"
                            );
                        }
                    } else {
                        tracing::debug!("proc supervision: no headers attached; leaving actor_id unchanged");
                    }

                    // Forward the supervision event to the ActorMesh (keyed by its mesh name)
                    // that registered for events in this ProcMesh. The routing table
                    // (actor_event_router) is keyed by ActorMeshName, which we obtain from
                    // actor_id.name(). If no matching mesh is found, log the current table
                    // to aid diagnosis.
                    let actor_id = event.actor_id.clone();
                    let actor_status = event.actor_status.clone();
                    let reason = event.to_string();
                    if let Some(tx) = self.actor_event_router.get(actor_id.name()) {
                        tracing::info!(
                            name = SupervisionEventState::SupervisionEventForwardFailed.as_ref(),
                            actor_id = %actor_id,
                            actor_name = actor_id.name(),
                            status = %actor_status,
                            "proc supervision: delivering event to registered ActorMesh"
                        );
                        if tx.send(event).is_err() {
                            tracing::warn!(
                                name = SupervisionEventState::SupervisionEventForwardFailed.as_ref(),
                                actor_id = %actor_id,
                                "proc supervision: registered ActorMesh dropped receiver; unable to deliver"
                            );
                        }
                    } else {
                        let registered_meshes: Vec<_> = self.actor_event_router.iter().map(|e| e.key().clone()).collect();
                        tracing::warn!(
                            name = SupervisionEventState::SupervisionEventForwardFailed.as_ref(),
                            actor_id = %actor_id,
                            "proc supervision: no ActorMesh registered for this actor {:?}", registered_meshes,
                        );
                    }
                    // Ensure we have a known rank for the proc
                    // containing this actor. If we don't, we can't
                    // attribute the failure to a known process.
                    let Some((rank, _)) = self.ranks.get(actor_id.proc_id()) else {
                        tracing::warn!(
                            actor_id = %actor_id,
                            "proc supervision: actor belongs to an unmapped proc; dropping event"
                        );
                        continue;
                    };

                    metrics::PROC_MESH_ACTOR_FAILURES.add(
                        1,
                        hyperactor_telemetry::kv_pairs!(
                            "actor_id" => actor_id.to_string(),
                            "rank" => rank.to_string(),
                            "status" => actor_status.to_string(),
                        ),
                    );

                    // Send this event to Python proc mesh to keep its
                    // health status up to date.
                    break Some(ProcEvent::Crashed(*rank, reason))
                }
            }
        }
    }

    pub fn into_alloc(self) -> Box<dyn Alloc + Send + Sync> {
        self.event_state.alloc
    }
}

/// Spawns from shared ([`Arc`]) proc meshes, providing [`ActorMesh`]es with
/// static lifetimes.
#[async_trait]
pub trait SharedSpawnable {
    async fn spawn<A: Actor + RemoteActor>(
        self,
        actor_name: &str,
        params: &A::Params,
    ) -> Result<RootActorMesh<'static, A>, anyhow::Error>
    where
        A::Params: RemoteMessage;
}

#[async_trait]
impl<D: Deref<Target = ProcMesh> + Send + Sync + 'static> SharedSpawnable for D {
    async fn spawn<A: Actor + RemoteActor>(
        self,
        actor_name: &str,
        params: &A::Params,
    ) -> Result<RootActorMesh<'static, A>, anyhow::Error>
    where
        A::Params: RemoteMessage,
    {
        let (tx, rx) = mpsc::unbounded_channel::<ActorSupervisionEvent>();
        {
            // Instantiate supervision routing BEFORE spawning the actor mesh.
            self.actor_event_router.insert(actor_name.to_string(), tx);
            tracing::info!(
                name = "router_insert",
                actor_name = %actor_name,
                "the length of the router is {}", self.actor_event_router.len(),
            );
        }
        let ranks =
            ProcMesh::spawn_on_procs::<A>(&self.client, self.agents(), actor_name, params).await?;
        Ok(RootActorMesh::new_shared(
            self,
            actor_name.to_string(),
            rx,
            ranks,
        ))
    }
}

#[async_trait]
impl Mesh for ProcMesh {
    type Node = ProcId;
    type Id = ProcMeshId;
    type Sliced<'a> = SlicedProcMesh<'a>;

    fn shape(&self) -> &Shape {
        &self.shape
    }

    fn select<R: Into<Range>>(
        &self,
        label: &str,
        range: R,
    ) -> Result<Self::Sliced<'_>, ShapeError> {
        Ok(SlicedProcMesh(self, self.shape().select(label, range)?))
    }

    fn get(&self, rank: usize) -> Option<ProcId> {
        Some(self.ranks[rank].1.clone())
    }

    fn id(&self) -> Self::Id {
        ProcMeshId(self.world_id().name().to_string())
    }
}

impl fmt::Display for ProcMesh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{ shape: {} }}", self.shape())
    }
}

impl fmt::Debug for ProcMesh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcMesh")
            .field("shape", &self.shape())
            .field("ranks", &self.ranks)
            .field("client_proc", &self.client_proc)
            .field("client", &self.client)
            // Skip the alloc field since it doesn't implement Debug
            .finish()
    }
}

pub struct SlicedProcMesh<'a>(&'a ProcMesh, Shape);

#[async_trait]
impl Mesh for SlicedProcMesh<'_> {
    type Node = ProcId;
    type Id = ProcMeshId;
    type Sliced<'b>
        = SlicedProcMesh<'b>
    where
        Self: 'b;

    fn shape(&self) -> &Shape {
        &self.1
    }

    fn select<R: Into<Range>>(
        &self,
        label: &str,
        range: R,
    ) -> Result<Self::Sliced<'_>, ShapeError> {
        Ok(Self(self.0, self.1.select(label, range)?))
    }

    fn get(&self, _index: usize) -> Option<ProcId> {
        unimplemented!()
    }

    fn id(&self) -> Self::Id {
        self.0.id()
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches::assert_matches;

    use hyperactor::actor::ActorStatus;
    use ndslice::extent;

    use super::*;
    use crate::actor_mesh::ActorMesh;
    use crate::actor_mesh::test_util::Error;
    use crate::actor_mesh::test_util::TestActor;
    use crate::alloc::AllocSpec;
    use crate::alloc::Allocator;
    use crate::alloc::local::LocalAllocator;
    use crate::sel_from_shape;

    #[tokio::test]
    async fn test_basic() {
        let alloc = LocalAllocator
            .allocate(AllocSpec {
                extent: extent!(replica = 4),
                constraints: Default::default(),
                proc_name: None,
            })
            .await
            .unwrap();

        let name = alloc.name().to_string();
        let mesh = ProcMesh::allocate(alloc).await.unwrap();

        assert_eq!(mesh.get(0).unwrap().world_name(), Some(name.as_str()));
    }

    #[tokio::test]
    async fn test_propagate_lifecycle_events() {
        let alloc = LocalAllocator
            .allocate(AllocSpec {
                extent: extent!(replica = 4),
                constraints: Default::default(),
                proc_name: None,
            })
            .await
            .unwrap();

        let stop = alloc.stopper();
        let monkey = alloc.chaos_monkey();
        let mut mesh = ProcMesh::allocate(alloc).await.unwrap();
        let mut events = mesh.events().unwrap();

        monkey(1, ProcStopReason::Killed(1, false));
        assert_matches!(
            events.next().await.unwrap(),
            ProcEvent::Stopped(1, ProcStopReason::Killed(1, false))
        );

        stop();
        for _ in 0..3 {
            assert_matches!(
                events.next().await.unwrap(),
                ProcEvent::Stopped(_, ProcStopReason::Stopped)
            );
        }
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn test_supervision_failure() {
        // For now, we propagate all actor failures to the proc.

        let alloc = LocalAllocator
            .allocate(AllocSpec {
                extent: extent!(replica = 2),
                constraints: Default::default(),
                proc_name: None,
            })
            .await
            .unwrap();
        let stop = alloc.stopper();
        let mut mesh = ProcMesh::allocate(alloc).await.unwrap();
        let mut events = mesh.events().unwrap();

        let mut actors = mesh.spawn::<TestActor>("failing", &()).await.unwrap();
        let mut actor_events = actors.events().unwrap();

        actors
            .cast(
                mesh.client(),
                sel_from_shape!(actors.shape(), replica = 0),
                Error("failmonkey".to_string()),
            )
            .unwrap();

        assert_matches!(
            events.next().await.unwrap(),
            ProcEvent::Crashed(0, reason) if reason.contains("failmonkey")
        );

        let mut event = actor_events.next().await.unwrap();
        assert_matches!(event.actor_status, ActorStatus::Failed(_));
        assert_eq!(event.actor_id.1, "failing".to_string());
        assert_eq!(event.actor_id.2, 0);

        stop();
        assert_matches!(
            events.next().await.unwrap(),
            ProcEvent::Stopped(0, ProcStopReason::Stopped),
        );
        assert_matches!(
            events.next().await.unwrap(),
            ProcEvent::Stopped(1, ProcStopReason::Stopped),
        );

        assert!(events.next().await.is_none());
        event = actor_events.next().await.unwrap();
        assert_matches!(event.actor_status, ActorStatus::Failed(_));
        assert_eq!(event.actor_id.2, 0);
    }

    #[timed_test::async_timed_test(timeout_secs = 5)]
    async fn test_spawn_twice() {
        let alloc = LocalAllocator
            .allocate(AllocSpec {
                extent: extent!(replica = 1),
                constraints: Default::default(),
                proc_name: None,
            })
            .await
            .unwrap();
        let mesh = ProcMesh::allocate(alloc).await.unwrap();

        mesh.spawn::<TestActor>("dup", &()).await.unwrap();
        let result = mesh.spawn::<TestActor>("dup", &()).await;
        assert!(result.is_err());
    }
}
