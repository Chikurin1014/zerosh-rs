pub struct Defer<F>
where
    F: Fn(),
{
    pub f: F,
}

impl<F> Drop for Defer<F>
where
    F: Fn(),
{
    fn drop(&mut self) {
        (self.f)();
    }
}
