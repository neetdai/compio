#[doc(no_inline)]
pub use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::{collections::VecDeque, io, time::Duration};

use io_uring::{
    cqueue,
    opcode::AsyncCancel,
    squeue,
    types::{SubmitArgs, Timespec},
    IoUring,
};
pub(crate) use libc::{sockaddr_storage, socklen_t};

use crate::driver::{Entry, Poller};

pub(crate) mod op;

/// Abstraction of io-uring operations.
pub trait OpCode {
    /// Create submission entry.
    fn create_entry(&mut self) -> squeue::Entry;
}

/// Low-level driver of io-uring.
pub struct Driver {
    inner: IoUring,
    squeue: VecDeque<squeue::Entry>,
}

impl Driver {
    const CANCEL: u64 = u64::MAX;

    /// Create a new io-uring driver with 1024 entries.
    pub fn new() -> io::Result<Self> {
        Self::with_entries(1024)
    }

    /// Create a new io-uring driver with specified entries.
    pub fn with_entries(entries: u32) -> io::Result<Self> {
        Ok(Self {
            inner: IoUring::new(entries)?,
            squeue: VecDeque::with_capacity(entries as usize),
        })
    }

    // Auto means that it choose to wait or not automatically.
    fn submit_auto(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        let res = if self.squeue.is_empty() {
            // Last part of submission queue, wait till timeout.
            if let Some(duration) = timeout {
                let timespec = timespec(duration);
                let args = SubmitArgs::new().timespec(&timespec);
                self.inner.submitter().submit_with_args(1, &args)
            } else {
                self.inner.submit_and_wait(1)
            }
        } else {
            self.inner.submit()
        };
        match res {
            Ok(_) => Ok(()),
            Err(e) => match e.raw_os_error() {
                Some(libc::ETIME) => Err(io::Error::from_raw_os_error(libc::ETIMEDOUT)),
                Some(libc::EBUSY) => Ok(()),
                _ => Err(e),
            },
        }
    }

    fn flush_submissions(&mut self) {
        let mut inner_squeue = self.inner.submission();
        if inner_squeue.is_full() {
            // can't flush
            return;
        }
        let remain_space = inner_squeue.capacity() - inner_squeue.len();
        if self.squeue.len() <= remain_space {
            // inner queue has enough space for all entries
            // use batched submission optimization
            let (s1, s2) = self.squeue.as_slices();
            unsafe {
                inner_squeue
                    .push_multiple(s1)
                    .expect("queue has enough space");
                inner_squeue
                    .push_multiple(s2)
                    .expect("queue has enough space");
            }
            self.squeue.clear();
        } else {
            // deque has more items than the IO ring could fit
            // push one by one
            for entry in self.squeue.drain(..remain_space) {
                unsafe { inner_squeue.push(&entry) }.expect("queue has enough space");
            }
        }
        inner_squeue.sync();
    }

    fn poll_entries(&mut self, entries: &mut impl Extend<Entry>) {
        let completed_entries = self.inner.completion().filter_map(|entry| {
            const SYSCALL_ECANCELED: i32 = -libc::ECANCELED;
            match (entry.user_data(), entry.result()) {
                // Cancel or cancelled operation.
                (Self::CANCEL, _) | (_, SYSCALL_ECANCELED) => None,
                (..) => Some(create_entry(entry)),
            }
        });
        entries.extend(completed_entries);
    }
}

impl Poller for Driver {
    fn attach(&mut self, _fd: RawFd) -> io::Result<()> {
        Ok(())
    }

    unsafe fn push(
        &mut self,
        op: &mut (impl OpCode + 'static),
        user_data: usize,
    ) -> io::Result<()> {
        let entry = op.create_entry().user_data(user_data as _);
        self.squeue.push_back(entry);
        Ok(())
    }

    fn cancel(&mut self, user_data: usize) {
        self.squeue.push_back(
            AsyncCancel::new(user_data as _)
                .build()
                .user_data(Self::CANCEL),
        );
    }

    fn poll(
        &mut self,
        timeout: Option<Duration>,
        entries: &mut impl Extend<Entry>,
    ) -> io::Result<()> {
        // Anyway we need to submit once, no matter there are entries in squeue.
        loop {
            self.flush_submissions();

            self.submit_auto(timeout)?;

            self.poll_entries(entries);

            if self.squeue.is_empty() && self.inner.submission().is_empty() {
                break;
            }
        }
        Ok(())
    }
}

impl AsRawFd for Driver {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

fn create_entry(entry: cqueue::Entry) -> Entry {
    let result = entry.result();
    let result = if result < 0 {
        Err(io::Error::from_raw_os_error(-result))
    } else {
        Ok(result as _)
    };
    Entry::new(entry.user_data() as _, result)
}

fn timespec(duration: std::time::Duration) -> Timespec {
    Timespec::new()
        .sec(duration.as_secs())
        .nsec(duration.subsec_nanos())
}
