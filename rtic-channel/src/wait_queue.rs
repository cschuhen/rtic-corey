//! ...

use core::marker::PhantomPinned;
use core::pin::Pin;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};
use core::task::Waker;
use critical_section as cs;

pub type WaitQueue = LinkedList<Waker>;

/// A FIFO linked list for a wait queue.
pub struct LinkedList<T> {
    head: AtomicPtr<Link<T>>, // UnsafeCell<*mut Link<T>>
    tail: AtomicPtr<Link<T>>,
}

impl<T> LinkedList<T> {
    /// Create a new linked list.
    pub const fn new() -> Self {
        Self {
            head: AtomicPtr::new(null_mut()),
            tail: AtomicPtr::new(null_mut()),
        }
    }
}

impl<T: Clone> LinkedList<T> {
    const R: Ordering = Ordering::Relaxed;

    /// Pop the first element in the queue.
    pub fn pop(&self) -> Option<T> {
        cs::with(|_| {
            // Make sure all previous writes are visible
            core::sync::atomic::fence(Ordering::SeqCst);

            let head = self.head.load(Self::R);

            // SAFETY: `as_ref` is safe as `insert` requires a valid reference to a link
            if let Some(head_ref) = unsafe { head.as_ref() } {
                // Move head to the next element
                self.head.store(head_ref.next.load(Self::R), Self::R);

                // We read the value at head
                let head_val = head_ref.val.clone();

                let tail = self.tail.load(Self::R);
                if head == tail {
                    // The queue is empty
                    self.tail.store(null_mut(), Self::R);
                }

                if let Some(next_ref) = unsafe { head_ref.next.load(Self::R).as_ref() } {
                    next_ref.prev.store(null_mut(), Self::R);
                }

                // Clear the pointers in the node.
                head_ref.next.store(null_mut(), Self::R);
                head_ref.prev.store(null_mut(), Self::R);

                return Some(head_val);
            }

            None
        })
    }

    /// Put an element at the back of the queue.
    pub fn push(&self, link: Pin<&mut Link<T>>) {
        cs::with(|_| {
            // Make sure all previous writes are visible
            core::sync::atomic::fence(Ordering::SeqCst);

            let tail = self.tail.load(Self::R);

            // SAFETY: This datastructure does not move the underlying value.
            let link = unsafe { link.get_unchecked_mut() };

            if let Some(tail_ref) = unsafe { tail.as_ref() } {
                // Queue is not empty
                link.prev.store(tail, Self::R);
                self.tail.store(link, Self::R);
                tail_ref.next.store(link, Self::R);
            } else {
                // Queue is empty
                self.tail.store(link, Self::R);
                self.head.store(link, Self::R);
            }
        });
    }

    /// Check if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.head.load(Self::R).is_null()
    }
}

/// A link in the linked list.
pub struct Link<T> {
    pub(crate) val: T,
    next: AtomicPtr<Link<T>>,
    prev: AtomicPtr<Link<T>>,
    _up: PhantomPinned,
}

impl<T: Clone> Link<T> {
    const R: Ordering = Ordering::Relaxed;

    /// Create a new link.
    pub const fn new(val: T) -> Self {
        Self {
            val,
            next: AtomicPtr::new(null_mut()),
            prev: AtomicPtr::new(null_mut()),
            _up: PhantomPinned,
        }
    }

    pub fn remove_from_list(&mut self, list: &LinkedList<T>) {
        cs::with(|_| {
            // Make sure all previous writes are visible
            core::sync::atomic::fence(Ordering::SeqCst);

            let prev = self.prev.load(Self::R);
            let next = self.next.load(Self::R);

            match unsafe { (prev.as_ref(), next.as_ref()) } {
                (None, None) => {
                    // Not in the list or alone in the list, check if list head == node address
                    let sp = self as *const _;

                    if sp == list.head.load(Ordering::Relaxed) {
                        list.head.store(null_mut(), Self::R);
                        list.tail.store(null_mut(), Self::R);
                    }
                }
                (None, Some(next_ref)) => {
                    // First in the list
                    next_ref.prev.store(null_mut(), Self::R);
                    list.head.store(next, Self::R);
                }
                (Some(prev_ref), None) => {
                    // Last in the list
                    prev_ref.next.store(null_mut(), Self::R);
                    list.tail.store(prev, Self::R);
                }
                (Some(prev_ref), Some(next_ref)) => {
                    // Somewhere in the list

                    // Connect the `prev.next` and `next.prev` with each other to remove the node
                    prev_ref.next.store(next, Self::R);
                    next_ref.prev.store(prev, Self::R);
                }
            }
        })
    }
}

#[cfg(test)]
impl<T: core::fmt::Debug + Clone> LinkedList<T> {
    fn print(&self) {
        cs::with(|_| {
            // Make sure all previous writes are visible
            core::sync::atomic::fence(Ordering::SeqCst);

            let mut head = self.head.load(Self::R);
            let tail = self.tail.load(Self::R);

            println!(
                "List - h = 0x{:x}, t = 0x{:x}",
                head as usize, tail as usize
            );

            let mut i = 0;

            // SAFETY: `as_ref` is safe as `insert` requires a valid reference to a link
            while let Some(head_ref) = unsafe { head.as_ref() } {
                println!(
                    "    {}: {:?}, s = 0x{:x}, n = 0x{:x}, p = 0x{:x}",
                    i,
                    head_ref.val,
                    head as usize,
                    head_ref.next.load(Ordering::Relaxed) as usize,
                    head_ref.prev.load(Ordering::Relaxed) as usize
                );

                head = head_ref.next.load(Self::R);

                i += 1;
            }
        });
    }
}

#[cfg(test)]
impl<T: core::fmt::Debug + Clone> Link<T> {
    fn print(&self) {
        cs::with(|_| {
            // Make sure all previous writes are visible
            core::sync::atomic::fence(Ordering::SeqCst);

            println!("Link:");

            println!(
                "    val = {:?}, n = 0x{:x}, p = 0x{:x}",
                self.val,
                self.next.load(Ordering::Relaxed) as usize,
                self.prev.load(Ordering::Relaxed) as usize
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_list() {
        let mut wq = LinkedList::<u32>::new();

        let mut i1 = Link::new(10);
        let mut i2 = Link::new(11);
        let mut i3 = Link::new(12);
        let mut i4 = Link::new(13);
        let mut i5 = Link::new(14);

        wq.push(unsafe { Pin::new_unchecked(&mut i1) });
        wq.push(unsafe { Pin::new_unchecked(&mut i2) });
        wq.push(unsafe { Pin::new_unchecked(&mut i3) });
        wq.push(unsafe { Pin::new_unchecked(&mut i4) });
        wq.push(unsafe { Pin::new_unchecked(&mut i5) });

        wq.print();

        wq.pop();
        i1.print();

        wq.print();

        i4.remove_from_list(&wq);

        wq.print();

        // i1.remove_from_list(&wq);
        // wq.print();

        println!("i2");
        i2.remove_from_list(&wq);
        wq.print();

        println!("i3");
        i3.remove_from_list(&wq);
        wq.print();

        println!("i5");
        i5.remove_from_list(&wq);
        wq.print();
    }
}
