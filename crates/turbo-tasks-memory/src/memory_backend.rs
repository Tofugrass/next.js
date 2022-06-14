use std::{cell::RefCell, collections::HashSet, future::Future, pin::Pin};

use anyhow::Result;
use event_listener::EventListener;
use flurry::HashMap as FHashMap;
use tokio::task::futures::TaskLocalFuture;
use turbo_tasks::{
    backend::{
        Backend, BackgroundJobId, PersistentTaskType, SlotContent, SlotMappings, TaskExecutionSpec,
        TransientTaskType,
    },
    util::{IdFactory, NoMoveVec},
    RawVc, TaskId, TurboTasksBackendApi,
};

use crate::{
    output::Output,
    task::{Task, DEPENDENCIES_TO_TRACK},
};

pub struct MemoryBackend {
    memory_tasks: NoMoveVec<Task, 13>,
    background_jobs: NoMoveVec<BackgroundJob>,
    background_job_id_factory: IdFactory<BackgroundJobId>,
    task_cache: FHashMap<PersistentTaskType, TaskId>,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self {
            memory_tasks: NoMoveVec::new(),
            background_jobs: NoMoveVec::new(),
            background_job_id_factory: IdFactory::new(),
            task_cache: FHashMap::new(),
        }
    }

    fn connect_task_child(
        &self,
        parent: TaskId,
        child: TaskId,
        turbo_tasks: &dyn TurboTasksBackendApi,
    ) {
        self.with_task(parent, |parent| {
            parent.connect_child(child, self, turbo_tasks)
        });
    }

    pub(crate) fn create_background_job(&self, job: BackgroundJob) -> BackgroundJobId {
        let id = self.background_job_id_factory.get();
        // SAFETY: This is a fresh id
        unsafe {
            self.background_jobs.insert(*id, job);
        }
        id
    }

    fn try_get_output<T, F: FnOnce(&mut Output) -> Result<T>>(
        &self,
        id: TaskId,
        func: F,
    ) -> Result<Result<T, EventListener>> {
        self.with_task(id, |task| task.get_or_wait_output(func))
    }

    pub fn with_all_cached_tasks(&self, mut func: impl FnMut(TaskId)) {
        for id in self.task_cache.pin().values() {
            func(*id);
        }
    }

    pub fn with_task<T>(&self, id: TaskId, func: impl FnOnce(&Task) -> T) -> T {
        func(&self.memory_tasks.get(*id).unwrap())
    }
}

impl Backend for MemoryBackend {
    fn invalidate_task(&self, task: TaskId, turbo_tasks: &dyn TurboTasksBackendApi) {
        self.with_task(task, |task| task.invalidate(self, turbo_tasks));
    }

    fn invalidate_tasks(&self, tasks: Vec<TaskId>, turbo_tasks: &dyn TurboTasksBackendApi) {
        for task in tasks.into_iter() {
            self.with_task(task, |task| {
                task.invalidate(self, turbo_tasks);
            });
        }
    }

    fn get_task_description(&self, task: TaskId) -> String {
        self.with_task(task, |task| task.get_description())
    }

    type ExecutionScopeFuture<T: Future<Output = ()> + Send + 'static> =
        TaskLocalFuture<RefCell<HashSet<RawVc>>, T>;
    fn execution_scope<T: Future<Output = ()> + Send + 'static>(
        &self,
        _task: TaskId,
        future: T,
    ) -> Self::ExecutionScopeFuture<T> {
        DEPENDENCIES_TO_TRACK.scope(Default::default(), future)
    }

    fn try_start_task_execution(
        &self,
        task: TaskId,
        turbo_tasks: &dyn TurboTasksBackendApi,
    ) -> Option<TaskExecutionSpec> {
        self.with_task(task, |task| {
            if task.execution_started(self, turbo_tasks) {
                let slot_mappings = task.take_slot_mappings();
                Some(TaskExecutionSpec {
                    slot_mappings: Some(slot_mappings),
                    future: task.execute(turbo_tasks),
                })
            } else {
                None
            }
        })
    }

    fn task_execution_completed(
        &self,
        task: TaskId,
        slot_mappings: Option<SlotMappings>,
        result: anyhow::Result<RawVc>,
        turbo_tasks: &dyn TurboTasksBackendApi,
    ) -> bool {
        self.with_task(task, |task| {
            task.execution_result(result, turbo_tasks);
            task.execution_completed(slot_mappings, self)
        })
    }

    fn try_read_task_output(
        &self,
        task: TaskId,
        reader: TaskId,
        _turbo_tasks: &dyn TurboTasksBackendApi,
    ) -> Result<Result<RawVc, EventListener>> {
        self.try_get_output(task, |output| {
            Task::add_dependency_to_current(RawVc::TaskOutput(task));
            output.read(reader)
        })
    }

    unsafe fn try_read_task_output_untracked(
        &self,
        task: TaskId,
        _turbo_tasks: &dyn TurboTasksBackendApi,
    ) -> Result<Result<RawVc, EventListener>> {
        self.try_get_output(task, |output| unsafe { output.read_untracked() })
    }

    fn track_read_task_output(
        &self,
        task: TaskId,
        reader: TaskId,
        _turbo_tasks: &dyn TurboTasksBackendApi,
    ) {
        self.with_task(task, |t| {
            t.with_output_mut(|output| {
                Task::add_dependency_to_current(RawVc::TaskOutput(task));
                output.track_read(reader);
            })
        })
    }

    fn try_read_task_slot(
        &self,
        task: TaskId,
        index: usize,
        reader: TaskId,
        _turbo_tasks: &dyn TurboTasksBackendApi,
    ) -> Result<Result<SlotContent, EventListener>> {
        Task::add_dependency_to_current(RawVc::TaskSlot(task, index));
        Ok(Ok(self.with_task(task, |task| {
            task.with_slot_mut(index, |slot| slot.read_content(reader))
        })))
    }

    unsafe fn try_read_task_slot_untracked(
        &self,
        task: TaskId,
        index: usize,
        _turbo_tasks: &dyn TurboTasksBackendApi,
    ) -> Result<Result<SlotContent, EventListener>> {
        Ok(Ok(self.with_task(task, |task| {
            task.with_slot(index, |slot| unsafe { slot.read_content_untracked() })
        })))
    }

    fn track_read_task_slot(
        &self,
        task: TaskId,
        index: usize,
        reader: TaskId,
        _turbo_tasks: &dyn TurboTasksBackendApi,
    ) {
        Task::add_dependency_to_current(RawVc::TaskSlot(task, index));
        self.with_task(task, |task| {
            task.with_slot_mut(index, |slot| slot.track_read(reader))
        });
    }

    fn get_fresh_slot(&self, task: TaskId, _turbo_tasks: &dyn TurboTasksBackendApi) -> usize {
        self.with_task(task, |task| task.get_fresh_slot())
    }

    fn update_task_slot(
        &self,
        task: TaskId,
        index: usize,
        content: SlotContent,
        turbo_tasks: &dyn TurboTasksBackendApi,
    ) {
        self.with_task(task, |task| {
            task.with_slot_mut(index, |slot| slot.assign(content, turbo_tasks))
        })
    }

    /// SAFETY: Must only called once with the same id
    fn run_background_job<'a>(
        &'a self,
        id: BackgroundJobId,
        turbo_tasks: &'a dyn TurboTasksBackendApi,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        // SAFETY: id will not be reused until with job is done
        if let Some(job) = unsafe { self.background_jobs.take(*id) } {
            Box::pin(async move {
                job.run(self, turbo_tasks).await;
                // SAFETY: This id will no longer be used
                unsafe {
                    self.background_job_id_factory.reuse(id);
                }
            })
        } else {
            Box::pin(async {})
        }
    }

    fn get_or_create_persistent_task(
        &self,
        task_type: PersistentTaskType,
        parent_task: TaskId,
        turbo_tasks: &dyn TurboTasksBackendApi,
    ) -> TaskId {
        let map = self.task_cache.pin();
        let result = if let Some(task) = map.get(&task_type).map(|guard| *guard) {
            // fast pass without creating a new task
            self.connect_task_child(parent_task, task, turbo_tasks);

            // TODO maybe force (background) scheduling to avoid inactive tasks hanging in
            // "in progress" until they become active
            task
        } else {
            // slow pass with key lock
            let id = turbo_tasks.get_fresh_task_id();
            let task = match &task_type {
                PersistentTaskType::Native(fn_id, inputs) => {
                    Task::new_native(id, inputs.clone(), *fn_id)
                }
                PersistentTaskType::ResolveNative(fn_id, inputs) => {
                    Task::new_resolve_native(id, inputs.clone(), *fn_id)
                }
                PersistentTaskType::ResolveTrait(trait_type, trait_fn_name, inputs) => {
                    Task::new_resolve_trait(id, *trait_type, trait_fn_name.clone(), inputs.clone())
                }
            };
            // SAFETY: We have a fresh task id where nobody knows about yet
            unsafe {
                self.memory_tasks.insert(*id, task);
            }
            let result_task = match map.try_insert(task_type, id) {
                Ok(_) => {
                    // This is the most likely case
                    id
                }
                Err(r) => {
                    // SAFETY: We have a fresh task id where nobody knows about yet
                    unsafe {
                        self.memory_tasks.remove(*id);
                        turbo_tasks.reuse_task_id(id);
                    }
                    *r.current
                }
            };
            self.connect_task_child(parent_task, result_task, turbo_tasks);
            result_task
        };
        // keep the guard alive over the whole function
        // to avoid load on GC
        drop(map);
        result
    }

    fn create_transient_task(
        &self,
        task_type: TransientTaskType,
        turbo_tasks: &dyn TurboTasksBackendApi,
    ) -> TaskId {
        let id = turbo_tasks.get_fresh_task_id();
        let task = match task_type {
            TransientTaskType::Root(f) => Task::new_root(id, move || {
                let future = f();
                future
            }),
            TransientTaskType::Once(f) => Task::new_once(id, f),
        };
        // SAFETY: We have a fresh task id where nobody knows about yet
        unsafe {
            self.memory_tasks.insert(*id, task);
        }
        id
    }
}

pub(crate) enum BackgroundJob {
    RemoveTasks(HashSet<TaskId>),
    DeactivateTasks(Vec<TaskId>),
}

impl BackgroundJob {
    async fn run(self, backend: &MemoryBackend, turbo_tasks: &dyn TurboTasksBackendApi) {
        match self {
            BackgroundJob::RemoveTasks(tasks) => {
                for id in tasks {
                    backend.with_task(id, |task| {
                        task.remove(backend, turbo_tasks);
                    });
                }
            }
            BackgroundJob::DeactivateTasks(tasks) => {
                Task::deactivate_tasks(tasks, backend, turbo_tasks);
            }
        }
    }
}
