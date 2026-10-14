//! A tiny assembler for the injected stubs: words plus branch labels resolved
//! at the end. A branch is built with a closure over its offset, so every
//! encoder in `legaia_code_hooks::mips` is usable; the delay slot is the next
//! word the caller pushes.

type Fixup = (usize, usize, Box<dyn Fn(i16) -> u32>);

pub struct Asm {
    w: Vec<u32>,
    fix: Vec<Fixup>,
    labels: Vec<Option<usize>>,
}

impl Default for Asm {
    fn default() -> Self {
        Self::new()
    }
}

impl Asm {
    pub fn new() -> Self {
        Self {
            w: Vec::new(),
            fix: Vec::new(),
            labels: Vec::new(),
        }
    }

    pub fn label(&mut self) -> usize {
        self.labels.push(None);
        self.labels.len() - 1
    }

    pub fn bind(&mut self, l: usize) {
        self.labels[l] = Some(self.w.len());
    }

    pub fn op(&mut self, word: u32) {
        self.w.push(word);
    }

    pub fn br(&mut self, l: usize, f: impl Fn(i16) -> u32 + 'static) {
        self.fix.push((self.w.len(), l, Box::new(f)));
        self.w.push(0);
    }

    pub fn finish(mut self) -> Vec<u32> {
        for (at, l, f) in &self.fix {
            let to = self.labels[*l].expect("unbound label");
            self.w[*at] = f((to as i32 - *at as i32 - 1) as i16);
        }
        self.w
    }
}
