#[cfg(feature = "std")]
#[derive(Debug)]
pub(crate) struct RwLock<T> {
    rwlock: std::sync::RwLock<T>,
}
#[cfg(not(feature = "std"))]
#[derive(Debug)]
pub(crate) struct RwLock<T> {
    rwlock: spin::RwLock<T>,
}

impl<T> RwLock<T> {
    #[cfg(feature = "std")]
    #[inline]
    pub(crate) const fn new(t: T) -> RwLock<T> {
        RwLock {
            rwlock: std::sync::RwLock::new(t),
        }
    }

    #[cfg(not(feature = "std"))]
    #[inline]
    pub(crate) const fn new(t: T) -> RwLock<T> {
        RwLock {
            rwlock: spin::RwLock::new(t),
        }
    }

    #[inline]
    #[cfg(feature = "std")]
    pub(crate) fn read(&self) -> std::sync::RwLockReadGuard<'_, T> {
        return self.rwlock.read().unwrap();
    }

    #[inline]
    #[cfg(not(feature = "std"))]
    pub(crate) fn read(&self) -> spin::RwLockReadGuard<'_, T> {
        return self.rwlock.read();
    }

    #[inline]
    #[cfg(feature = "std")]
    pub(crate) fn write(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        return self.rwlock.write().unwrap();
    }

    #[inline]
    #[cfg(not(feature = "std"))]
    pub(crate) fn write(&self) -> spin::RwLockWriteGuard<'_, T> {
        return self.rwlock.write();
    }
}
