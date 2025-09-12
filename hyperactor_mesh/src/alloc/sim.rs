/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Support for allocating procs in the local process with simulated channels.

#![allow(dead_code)] // until it is used outside of testing

use std::collections::HashMap;

use async_trait::async_trait;
use hyperactor::ProcId;
use hyperactor::WorldId;
use hyperactor::channel::ChannelAddr;
use hyperactor::channel::ChannelTransport;
use hyperactor::mailbox::MailboxServerHandle;
use hyperactor::proc::Proc;
use ndslice::Point;
use ndslice::view::Extent;

use super::ProcStopReason;
use crate::alloc::Alloc;
use crate::alloc::AllocSpec;
use crate::alloc::Allocator;
use crate::alloc::AllocatorError;
use crate::alloc::LocalAlloc;
use crate::alloc::ProcState;
use crate::shortuuid::ShortUuid;

/// An allocator that runs procs in the local process with network traffic going through simulated channels.
/// Other than transport, the underlying implementation is an inner LocalAlloc.
pub struct SimAllocator;

#[async_trait]
impl Allocator for SimAllocator {
    type Alloc = SimAlloc;

    async fn allocate(&mut self, spec: AllocSpec) -> Result<Self::Alloc, AllocatorError> {
        Ok(SimAlloc::new(spec))
    }
}

impl SimAllocator {
    #[cfg(test)]
    pub(crate) fn new_and_start_simnet() -> Self {
        hyperactor::simnet::start();
        Self
    }
}

struct SimProc {
    proc: Proc,
    addr: ChannelAddr,
    handle: MailboxServerHandle,
}

/// A simulated allocation. It is a collection of procs that are running in the local process.
pub struct SimAlloc {
    inner: LocalAlloc,
    created: HashMap<ShortUuid, Point>,
}

impl SimAlloc {
    fn new(spec: AllocSpec) -> Self {
        let inner = LocalAlloc::new_with_transport(
            spec,
            ChannelTransport::Sim(Box::new(ChannelTransport::Unix)),
        );
        let client_proc_id = ProcId::Ranked(WorldId(format!("{}_manager", inner.name())), 0);

        let ext = inner.extent();

        hyperactor::simnet::simnet_handle()
            .expect("simnet event loop not running")
            .register_proc(
                client_proc_id.clone(),
                ext.point(ext.sizes().iter().map(|_| 0).collect())
                    .expect("should be valid point"),
            );

        Self {
            inner,
            created: HashMap::new(),
        }
    }
    /// A chaos monkey that can be used to stop procs at random.
    pub(crate) fn chaos_monkey(&self) -> impl Fn(usize, ProcStopReason) + 'static {
        self.inner.chaos_monkey()
    }

    /// A function to shut down the alloc for testing purposes.
    pub(crate) fn stopper(&self) -> impl Fn() + 'static {
        self.inner.stopper()
    }

    pub(crate) fn name(&self) -> &ShortUuid {
        self.inner.name()
    }

    fn size(&self) -> usize {
        self.inner.size()
    }
}

#[async_trait]
impl Alloc for SimAlloc {
    async fn next(&mut self) -> Option<ProcState> {
        let proc_state = self.inner.next().await?;
        match &proc_state {
            ProcState::Created {
                create_key, point, ..
            } => {
                self.created.insert(create_key.clone(), point.clone());
            }
            ProcState::Running {
                create_key,
                proc_id,
                ..
            } => {
                hyperactor::simnet::simnet_handle()
                    .expect("simnet event loop not running")
                    .register_proc(
                        proc_id.clone(),
                        self.created
                            .remove(create_key)
                            .expect("have point for create key"),
                    );
            }
            _ => (),
        }
        Some(proc_state)
    }

    fn spec(&self) -> &AllocSpec {
        self.inner.spec()
    }

    fn extent(&self) -> &Extent {
        self.inner.extent()
    }

    fn world_id(&self) -> &WorldId {
        self.inner.world_id()
    }

    fn transport(&self) -> ChannelTransport {
        self.inner.transport()
    }

    async fn stop(&mut self) -> Result<(), AllocatorError> {
        self.inner.stop().await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use hyperactor::simnet::BetaDistribution;
    use hyperactor::simnet::LatencyConfig;
    use hyperactor::simnet::LatencyDistribution;
    use ndslice::extent;

    use super::*;
    use crate::ProcMesh;
    use crate::RootActorMesh;
    use crate::actor_mesh::ActorMesh;
    use crate::alloc::AllocConstraints;
    use crate::alloc::test_utils::TestActor;

    #[tokio::test]
    async fn test_allocator_basic() {
        hyperactor::simnet::start();
        crate::alloc::testing::test_allocator_basic(SimAllocator).await;
    }

    #[tokio::test]
    async fn test_allocator_registers_resources() {
        hyperactor::simnet::start_with_config(LatencyConfig {
            inter_zone_distribution: LatencyDistribution::Beta(
                BetaDistribution::new(
                    tokio::time::Duration::from_millis(999),
                    tokio::time::Duration::from_millis(999),
                    1.0,
                    1.0,
                )
                .unwrap(),
            ),
            ..Default::default()
        });

        let alloc = SimAllocator
            .allocate(AllocSpec {
                extent: extent!(region = 1, dc = 1, zone = 10, rack = 1, host = 1, gpu = 1),
                constraints: AllocConstraints {
                    match_labels: HashMap::new(),
                },
                proc_name: None,
            })
            .await
            .unwrap();

        let proc_mesh = ProcMesh::allocate(alloc).await.unwrap();

        let handle = hyperactor::simnet::simnet_handle().unwrap();
        let actor_mesh: RootActorMesh<TestActor> = proc_mesh.spawn("echo", &()).await.unwrap();
        let actors = actor_mesh.iter_actor_refs().collect::<Vec<_>>();
        assert_eq!(
            handle.sample_latency(
                actors[0].actor_id().proc_id(),
                actors[1].actor_id().proc_id()
            ),
            tokio::time::Duration::from_millis(999)
        );
        assert_eq!(
            handle.sample_latency(
                actors[2].actor_id().proc_id(),
                actors[9].actor_id().proc_id()
            ),
            tokio::time::Duration::from_millis(999)
        );
        assert_eq!(
            handle.sample_latency(
                proc_mesh.client().actor_id().proc_id(),
                actors[1].actor_id().proc_id()
            ),
            tokio::time::Duration::from_millis(999)
        );
    }
}
