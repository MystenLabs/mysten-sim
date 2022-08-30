pub struct Handle {}

pub struct Runtime {}

pub struct EnterGuard<'a> {
    _lt: std::marker::PhantomData<&'a Handle>,
}

impl Drop for EnterGuard {
    fn drop(self) {
        todo!()
    }
}

impl Runtime {
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        todo!()
    }

    pub fn enter(&self) -> EnterGuard<'_> {
        self.handle.enter()
    }
}

pub struct Builder {}

impl Builder {
    pub fn new() {
        todo!();
    }

    pub fn new_multi_threaded() {
        todo!();
    }
}
