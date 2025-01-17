use anyhow::Result;

use super::env::SourceBlock;
use crate::error::UnexpectedEof;

#[derive(Default)]
pub struct Lexer {
    blocks: Vec<SourceBlockState>,
}

impl Lexer {
    pub fn push_source_block(&mut self, block: SourceBlock) {
        self.blocks.push(SourceBlockState::from(block));
    }

    pub fn pop_source_block(&mut self) -> bool {
        self.blocks.pop().is_some()
    }

    pub fn get_position(&self) -> Option<LexerPosition<'_>> {
        let offset = self.blocks.len();
        let input = self.blocks.last()?;
        Some(LexerPosition {
            offset,
            source_block_name: input.block.name(),
            line: &input.line,
            line_offset_start: std::cmp::min(input.prev_line_offset + 1, input.line_offset),
            line_offset_end: input.line_offset,
            line_number: input.line_number.unwrap_or_default(),
        })
    }

    pub fn scan_word(&mut self) -> Result<Option<Token<'_>>> {
        let Some(input) = self.blocks.last_mut() else {
            return Ok(None);
        };
        input.scan_word()
    }

    pub fn scan_until_space_or_eof(&mut self) -> Result<Token<'_>> {
        if let Some(input) = self.blocks.last_mut() {
            if let Some(word) = input.scan_word()? {
                return Ok(word);
            }
        }
        Ok(Token { data: "" })
    }

    pub fn scan_until_delimiter(&mut self, delimiter: char) -> Result<Token<'_>> {
        if let Some(token) = self.use_last_block()?.scan_until(delimiter)? {
            Ok(token)
        } else if delimiter as u32 == 0 {
            Ok(Token { data: "" })
        } else {
            anyhow::bail!(UnexpectedEof)
        }
    }

    pub fn scan_until<P: Delimiter>(&mut self, p: P) -> Result<Token<'_>> {
        if let Some(token) = self.use_last_block()?.scan_until(p)? {
            Ok(token)
        } else {
            anyhow::bail!(UnexpectedEof)
        }
    }

    pub fn rewind(&mut self, offset: usize) {
        if let Some(input) = self.blocks.last_mut() {
            input.rewind(offset)
        }
    }

    pub fn scan_skip_whitespace(&mut self) -> Result<()> {
        if let Some(input) = self.blocks.last_mut() {
            input.skip_whitespace()
        } else {
            Ok(())
        }
    }

    pub fn skip_line_whitespace(&mut self) {
        self.skip_while(char::is_whitespace)
    }

    pub fn skip_until<P: Delimiter>(&mut self, mut p: P) {
        if let Some(input) = self.blocks.last_mut() {
            input.skip_until(|c| !p.delim(c))
        }
    }

    pub fn skip_symbol(&mut self) {
        if let Some(input) = self.blocks.last_mut() {
            input.skip_symbol();
        }
    }

    pub fn skip_while<P: Delimiter>(&mut self, p: P) {
        if let Some(input) = self.blocks.last_mut() {
            input.skip_while(p)
        }
    }

    fn use_last_block(&mut self) -> Result<&mut SourceBlockState> {
        self.blocks.last_mut().ok_or_else(|| UnexpectedEof.into())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LexerPosition<'a> {
    pub offset: usize,
    pub source_block_name: &'a str,
    pub line: &'a str,
    pub line_offset_start: usize,
    pub line_offset_end: usize,
    pub line_number: usize,
}

pub struct Token<'a> {
    pub data: &'a str,
}

impl Token<'_> {
    pub fn subtokens(&self) -> Subtokens {
        Subtokens(self.data)
    }

    pub fn delta(&self, subtoken: &str) -> usize {
        self.data.len() - subtoken.len()
    }
}

pub struct Subtokens<'a>(&'a str);

impl<'a> Iterator for Subtokens<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let (i, _) = self.0.char_indices().next_back()?;
        let res = self.0;
        self.0 = &res[..i];
        Some(res)
    }
}

pub trait Delimiter {
    fn delim(&mut self, c: char) -> bool;
}

impl<T: FnMut(char) -> bool> Delimiter for T {
    fn delim(&mut self, c: char) -> bool {
        (self)(c)
    }
}

impl Delimiter for char {
    #[inline]
    fn delim(&mut self, c: char) -> bool {
        *self == c
    }
}

struct SourceBlockState {
    block: SourceBlock,
    line: String,
    line_offset: usize,
    prev_line_offset: usize,
    line_number: Option<usize>,
}

impl From<SourceBlock> for SourceBlockState {
    fn from(block: SourceBlock) -> Self {
        Self {
            block,
            line: Default::default(),
            line_offset: 0,
            prev_line_offset: 0,
            line_number: None,
        }
    }
}

impl SourceBlockState {
    fn scan_word(&mut self) -> Result<Option<Token<'_>>> {
        self.prev_line_offset = self.line_offset;

        loop {
            if (self.line.is_empty() || self.line_offset >= self.line.len()) && !self.read_line()? {
                return Ok(None);
            }

            self.skip_line_whitespace();
            let start = self.line_offset;
            self.skip_until(char::is_whitespace);
            let end = self.line_offset;

            if start == end {
                continue;
            }

            return Ok(Some(Token {
                data: &self.line[start..end],
            }));
        }
    }

    fn scan_until<P: Delimiter>(&mut self, mut p: P) -> Result<Option<Token<'_>>> {
        self.prev_line_offset = self.line_offset;

        if (self.line.is_empty() || self.line_offset >= self.line.len()) && !self.read_line()? {
            return Ok(None);
        }

        let start = self.line_offset;

        let mut found = false;
        self.skip_until(|c| {
            found |= p.delim(c);
            found
        });

        let end = self.line_offset;

        Ok(if found && end >= start {
            self.skip_symbol();
            Some(Token {
                data: &self.line[start..end],
            })
        } else {
            None
        })
    }

    fn rewind(&mut self, offset: usize) {
        self.line_offset -= offset;
    }

    fn skip_whitespace(&mut self) -> Result<()> {
        self.prev_line_offset = self.line_offset;

        loop {
            if (self.line.is_empty() || self.line_offset >= self.line.len()) && !self.read_line()? {
                return Ok(());
            }

            self.skip_line_whitespace();
            if self.line_offset < self.line.len() {
                return Ok(());
            }
        }
    }

    fn skip_line_whitespace(&mut self) {
        self.skip_while(char::is_whitespace)
    }

    fn skip_until<P: Delimiter>(&mut self, mut p: P) {
        self.skip_while(|c| !p.delim(c));
    }

    fn skip_symbol(&mut self) {
        let mut first = true;
        self.skip_while(|_| std::mem::take(&mut first))
    }

    fn skip_while<P: Delimiter>(&mut self, mut p: P) {
        let prev_offset = self.line_offset;
        for (offset, c) in self.line[self.line_offset..].char_indices() {
            if !p.delim(c) {
                self.line_offset = prev_offset + offset;
                return;
            }
        }
        self.line_offset = self.line.len();
    }

    fn read_line(&mut self) -> Result<bool> {
        self.prev_line_offset = 0;
        self.line_offset = 0;
        self.line.clear();
        let n = self.block.buffer_mut().read_line(&mut self.line)?;

        if let Some(line_number) = &mut self.line_number {
            *line_number += 1;
        } else {
            self.line_number = Some(0);
        }

        Ok(n > 0)
    }
}
