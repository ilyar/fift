use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use num_bigint::BigInt;

use super::{Context, Dictionary, Stack, StackValue, StackValueType, WordList};
use crate::util::*;

pub type Cont = Rc<dyn ContImpl>;

pub trait ContImpl {
    fn run(self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>>;

    fn up(&self) -> Option<&Cont> {
        None
    }

    fn fmt_name(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result;

    fn fmt_dump(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.fmt_name(d, f)
    }
}

impl dyn ContImpl + '_ {
    pub fn display_backtrace<'a>(&'a self, d: &'a Dictionary) -> impl std::fmt::Display + 'a {
        struct ContinuationBacktrace<'a> {
            d: &'a Dictionary,
            cont: &'a dyn ContImpl,
        }

        impl std::fmt::Display for ContinuationBacktrace<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let mut cont = self.cont;
                let mut newline = "";
                for i in 1..=16 {
                    write!(f, "{newline}level {i}: {}", cont.display_dump(self.d))?;
                    newline = "\n";
                    match cont.up() {
                        Some(next) => cont = next.as_ref(),
                        None => return Ok(()),
                    }
                }
                write!(f, "{newline}... more levels ...")
            }
        }

        ContinuationBacktrace { d, cont: self }
    }

    pub fn display_name<'a>(&'a self, d: &'a Dictionary) -> impl std::fmt::Display + 'a {
        struct ContinuationWriteName<'a> {
            d: &'a Dictionary,
            cont: &'a dyn ContImpl,
        }

        impl std::fmt::Display for ContinuationWriteName<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.cont.fmt_name(self.d, f)
            }
        }

        ContinuationWriteName { d, cont: self }
    }

    pub fn display_dump<'a>(&'a self, d: &'a Dictionary) -> impl std::fmt::Display + 'a {
        struct ContinuationDump<'a> {
            d: &'a Dictionary,
            cont: &'a dyn ContImpl,
        }

        impl std::fmt::Display for ContinuationDump<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.cont.fmt_dump(self.d, f)
            }
        }

        ContinuationDump { d, cont: self }
    }
}

pub struct InterpreterCont;

impl ContImpl for InterpreterCont {
    fn run(self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        thread_local! {
            static COMPILE_EXECUTE: Cont = Rc::new(CompileExecuteCont);
            static WORD: RefCell<String> = RefCell::new(String::with_capacity(128));
        };

        ctx.stdout.flush()?;

        let compile_exec = COMPILE_EXECUTE.with(|c| c.clone());

        'source_block: loop {
            'token: {
                let mut rewind = 0;
                let entry = 'entry: {
                    let Some(token) = ctx.input.scan_word()? else {
                        if ctx.input.pop_source_block() {
                            continue 'source_block;
                        }
                        return Ok(None);
                    };

                    // Find the largest subtoken first
                    for subtoken in token.subtokens() {
                        if let Some(entry) = ctx.dictionary.lookup(subtoken) {
                            rewind = token.delta(subtoken);
                            break 'entry entry;
                        }
                    }

                    // Find in predefined entries
                    if let Some(entry) = WORD.with(|word| {
                        let mut word = word.borrow_mut();
                        word.clear();
                        word.push_str(token.data);
                        word.push(' ');
                        ctx.dictionary.lookup(&word)
                    }) {
                        break 'entry entry;
                    }

                    // Try parse as number
                    if let Some(value) = ImmediateInt::try_from_str(token.data)? {
                        ctx.stack.push(value.num)?;
                        if let Some(denom) = value.denom {
                            ctx.stack.push(denom)?;
                            ctx.stack.push_argcount(2, ctx.dictionary.make_nop())?;
                        } else {
                            ctx.stack.push_argcount(1, ctx.dictionary.make_nop())?;
                        }
                        break 'token;
                    }

                    anyhow::bail!("Undefined word `{}`", token.data);
                };
                ctx.input.rewind(rewind);

                if entry.active {
                    ctx.next = SeqCont::make(
                        Some(compile_exec),
                        SeqCont::make(Some(self), ctx.next.take()),
                    );
                    return Ok(Some(entry.definition.clone()));
                } else {
                    ctx.stack.push_argcount(0, entry.definition.clone())?;
                }
            };

            ctx.exit_interpret.store(Box::new(
                ctx.next
                    .clone()
                    .unwrap_or_else(|| ctx.dictionary.make_nop()),
            ));

            ctx.next = SeqCont::make(Some(self), ctx.next.take());
            break Ok(Some(compile_exec));
        }
    }

    fn fmt_name(&self, _: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<text interpreter continuation>")
    }
}

struct CompileExecuteCont;

impl ContImpl for CompileExecuteCont {
    fn run(self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        Ok(if ctx.state.is_compile() {
            ctx.compile_stack_top()?;
            None
        } else {
            Some(ctx.execute_stack_top()?)
        })
    }

    fn fmt_name(&self, _: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<compile execute continuation>")
    }
}

pub struct ListCont {
    pub list: Rc<WordList>,
    pub after: Option<Cont>,
    pub pos: usize,
}

impl ContImpl for ListCont {
    fn run(mut self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        let is_last = self.pos + 1 >= self.list.items.len();
        let Some(current) = self.list.items.get(self.pos).cloned() else {
            return Ok(ctx.next.take())
        };

        match Rc::get_mut(&mut self) {
            Some(this) => {
                ctx.insert_before_next(&mut this.after);
                this.pos += 1;
                ctx.next = if is_last {
                    this.after.take()
                } else {
                    Some(self)
                };
            }
            None => {
                if let Some(next) = ctx.next.take() {
                    ctx.next = Some(Rc::new(ListCont {
                        after: SeqCont::make(self.after.clone(), Some(next)),
                        list: self.list.clone(),
                        pos: self.pos + 1,
                    }))
                } else if is_last {
                    ctx.next = self.after.clone()
                } else {
                    ctx.next = Some(Rc::new(ListCont {
                        after: self.after.clone(),
                        list: self.list.clone(),
                        pos: self.pos + 1,
                    }))
                }
            }
        }

        Ok(Some(current))
    }

    fn up(&self) -> Option<&Cont> {
        self.after.as_ref()
    }

    fn fmt_name(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_cont_name(self, d, f)
    }

    fn fmt_dump(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.pos == 0 {
            f.write_str("{")?;
            for item in &self.list.items {
                write!(f, " {}", item.display_name(d))?;
            }
            f.write_str(" }")
        } else {
            const N: usize = 16;

            if let Some(name) = d.resolve_name(self) {
                write!(f, "[in {name}:] ")?;
            }

            let len = self.list.items.len();
            let start = if self.pos >= N { self.pos - N } else { 0 };
            let items = self.list.items.iter();

            if start > 0 {
                f.write_str("... ")?;
            }
            for (i, item) in items.enumerate().skip(start).take(N) {
                if i == self.pos {
                    f.write_str("**HERE** ")?;
                }
                write!(f, "{} ", item.display_name(d))?;
            }
            if self.pos + N < len {
                f.write_str("...")?;
            }
            Ok(())
        }
    }
}

pub struct SeqCont {
    pub first: Option<Cont>,
    pub second: Option<Cont>,
}

impl SeqCont {
    pub fn make(first: Option<Cont>, second: Option<Cont>) -> Option<Cont> {
        if second.is_none() {
            first
        } else {
            Some(Rc::new(Self { first, second }))
        }
    }
}

impl ContImpl for SeqCont {
    fn run(mut self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        Ok(match Rc::get_mut(&mut self) {
            Some(this) => {
                if ctx.next.is_none() {
                    ctx.next = this.second.take();
                    this.first.take()
                } else {
                    let result = std::mem::replace(&mut this.first, this.second.take());
                    this.second = ctx.next.take();
                    ctx.next = Some(self);
                    result
                }
            }
            None => {
                ctx.next = SeqCont::make(self.second.clone(), ctx.next.take());
                self.first.clone()
            }
        })
    }

    fn up(&self) -> Option<&Cont> {
        self.second.as_ref()
    }

    fn fmt_name(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(first) = &self.first {
            first.as_ref().fmt_name(d, f)
        } else {
            Ok(())
        }
    }

    fn fmt_dump(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(first) = &self.first {
            first.as_ref().fmt_dump(d, f)?;
        }
        Ok(())
    }
}

pub struct TimesCont {
    pub body: Option<Cont>,
    pub after: Option<Cont>,
    pub count: usize,
}

impl ContImpl for TimesCont {
    fn run(mut self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        Ok(match Rc::get_mut(&mut self) {
            Some(this) => {
                ctx.insert_before_next(&mut this.after);

                if this.count > 1 {
                    this.count -= 1;
                    let body = this.body.clone();
                    ctx.next = Some(self);
                    body
                } else {
                    ctx.next = this.after.take();
                    this.body.take()
                }
            }
            None => {
                let next = SeqCont::make(self.after.clone(), ctx.next.take());

                ctx.next = if self.count > 1 {
                    Some(Rc::new(Self {
                        body: self.body.clone(),
                        after: next,
                        count: self.count - 1,
                    }))
                } else {
                    next
                };

                self.body.clone()
            }
        })
    }

    fn up(&self) -> Option<&Cont> {
        self.after.as_ref()
    }

    fn fmt_name(&self, _: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<repeat {} times>", self.count)
    }

    fn fmt_dump(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<repeat {} times:> ", self.count)?;
        if let Some(body) = &self.body {
            ContImpl::fmt_dump(body.as_ref(), d, f)?;
        }
        Ok(())
    }
}

pub struct UntilCont {
    pub body: Option<Cont>,
    pub after: Option<Cont>,
}

impl ContImpl for UntilCont {
    fn run(mut self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        if ctx.stack.pop_bool()? {
            return Ok(match Rc::get_mut(&mut self) {
                Some(this) => this.after.take(),
                None => self.after.clone(),
            });
        }

        let body = self.body.clone();
        let next = match Rc::get_mut(&mut self) {
            Some(this) => {
                ctx.insert_before_next(&mut this.after);
                self
            }
            None => {
                if let Some(next) = ctx.next.take() {
                    Rc::new(UntilCont {
                        body: self.body.clone(),
                        after: SeqCont::make(self.after.clone(), Some(next)),
                    })
                } else {
                    self
                }
            }
        };
        ctx.next = Some(next);
        Ok(body)
    }

    fn up(&self) -> Option<&Cont> {
        self.after.as_ref()
    }

    fn fmt_name(&self, _: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<until loop continuation>")
    }

    fn fmt_dump(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<until loop continuation:> ")?;
        if let Some(body) = &self.body {
            ContImpl::fmt_dump(body.as_ref(), d, f)?;
        }
        Ok(())
    }
}

pub struct WhileCont {
    pub condition: Option<Cont>,
    pub body: Option<Cont>,
    pub after: Option<Cont>,
    pub running_body: bool,
}

impl WhileCont {
    fn stage_name(&self) -> &'static str {
        if self.running_body {
            "body"
        } else {
            "condition"
        }
    }
}

impl ContImpl for WhileCont {
    fn run(mut self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        let cont = if self.running_body {
            if !ctx.stack.pop_bool()? {
                return Ok(match Rc::get_mut(&mut self) {
                    Some(this) => this.after.take(),
                    None => self.after.clone(),
                });
            }

            self.body.clone()
        } else {
            self.condition.clone()
        };

        let next = match Rc::get_mut(&mut self) {
            Some(this) => {
                ctx.insert_before_next(&mut this.after);
                this.running_body = !this.running_body;
                self
            }
            None => Rc::new(Self {
                condition: self.condition.clone(),
                body: self.body.clone(),
                after: SeqCont::make(self.after.clone(), ctx.next.take()),
                running_body: !self.running_body,
            }),
        };

        ctx.next = Some(next);
        Ok(cont)
    }

    fn up(&self) -> Option<&Cont> {
        self.after.as_ref()
    }

    fn fmt_name(&self, _: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<while loop {}>", self.stage_name())
    }

    fn fmt_dump(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<while loop {}:>", self.stage_name())?;
        let stage = if self.running_body {
            self.body.as_ref()
        } else {
            self.condition.as_ref()
        };
        if let Some(stage) = stage {
            ContImpl::fmt_dump(stage.as_ref(), d, f)?;
        }
        Ok(())
    }
}

pub struct IntLitCont(BigInt);

impl From<i32> for IntLitCont {
    fn from(value: i32) -> Self {
        Self(BigInt::from(value))
    }
}

impl ContImpl for IntLitCont {
    fn run(self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        let value = match Rc::try_unwrap(self) {
            Ok(value) => value.0,
            Err(this) => this.0.clone(),
        };
        ctx.stack.push(value)?;
        Ok(None)
    }

    fn fmt_name(&self, _: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub struct LitCont(pub Box<dyn StackValue>);

impl ContImpl for LitCont {
    fn run(self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        let value = match Rc::try_unwrap(self) {
            Ok(value) => value.0,
            Err(this) => this.0.clone(),
        };
        ctx.stack.push_raw(value)?;
        Ok(None)
    }

    fn fmt_name(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_lit_cont_name(self.0.as_ref(), d, f)
    }
}

pub struct MultiLitCont(pub Vec<Box<dyn StackValue>>);

impl ContImpl for MultiLitCont {
    fn run(self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        match Rc::try_unwrap(self) {
            Ok(value) => {
                for item in value.0 {
                    ctx.stack.push_raw(item)?;
                }
            }
            Err(this) => {
                for item in &this.0 {
                    ctx.stack.push_raw(item.clone())?;
                }
            }
        };
        Ok(None)
    }

    fn fmt_name(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for item in &self.0 {
            if first {
                first = false;
            } else {
                f.write_str(" ")?;
            }
            write_lit_cont_name(item.as_ref(), d, f)?;
        }
        Ok(())
    }
}

pub type ContextWordFunc = fn(&mut Context) -> Result<()>;

impl ContImpl for ContextWordFunc {
    fn run(self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        (self)(ctx)?;
        Ok(None)
    }

    fn fmt_name(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_cont_name(self, d, f)
    }
}

pub type ContextTailWordFunc = fn(&mut Context) -> Result<Option<Cont>>;

impl ContImpl for ContextTailWordFunc {
    fn run(self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        (self)(ctx)
    }

    fn fmt_name(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_cont_name(self, d, f)
    }
}

pub type StackWordFunc = fn(&mut Stack) -> Result<()>;

impl ContImpl for StackWordFunc {
    fn run(self: Rc<Self>, ctx: &mut Context) -> Result<Option<Cont>> {
        (self)(&mut ctx.stack)?;
        Ok(None)
    }

    fn fmt_name(&self, d: &Dictionary, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_cont_name(self, d, f)
    }
}

/// === impl Context ===

impl Context<'_> {
    fn insert_before_next(&mut self, cont: &mut Option<Cont>) {
        if let Some(next) = self.next.take() {
            *cont = match cont.take() {
                Some(prev) => Some(Rc::new(SeqCont {
                    first: Some(prev),
                    second: Some(next),
                })),
                None => Some(next),
            };
        }
    }
}

fn write_lit_cont_name(
    stack_entry: &dyn StackValue,
    d: &Dictionary,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    let ty = stack_entry.ty();
    match ty {
        StackValueType::Int | StackValueType::String | StackValueType::Builder => {
            stack_entry.fmt_dump(f)
        }
        _ => {
            if let Ok(cont) = stack_entry.as_cont() {
                write!(f, "{{ {} }}", cont.display_dump(d))
            } else {
                write!(f, "<literal of type {:?}>", ty)
            }
        }
    }
}

fn write_cont_name(
    cont: &dyn ContImpl,
    d: &Dictionary,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    if let Some(name) = d.resolve_name(cont) {
        f.write_str(name.trim_end())
    } else {
        write!(f, "<continuation {:?}>", cont as *const dyn ContImpl)
    }
}
