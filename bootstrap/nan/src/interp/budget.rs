//! The cells, chunks and allocations of an interpreter run: draws charge whole chunks to every
//! cell of a chain, and a chunk returns once no allocation in it is live.

/// The bytes of a chunk.
pub const CHUNK: u64 = 65_536;

struct Cell {
    limit: u128,
    charge: u128,
    parent: Option<u32>,
    open: bool,
    /// The chunk last drawn through it, where its next small allocation goes when it fits.
    last: Option<u32>,
}

struct Chunk {
    /// The cell it was drawn through.
    cell: u32,
    used: u64,
    live: u32,
}

#[derive(Default)]
pub struct Budgets {
    cells: Vec<Cell>,
    chunks: Vec<Chunk>,
    /// The chunks each allocation lies in.
    allocations: Vec<Vec<u32>>,
}

impl Budgets {
    /// A new root cell with a limit.
    pub fn root(&mut self, limit: u128) -> u32 {
        self.cell(limit, None)
    }

    /// A new open cell with a limit, whose parent is `parent`.
    pub fn split(&mut self, parent: u32, limit: u128) -> u32 {
        self.cell(limit, Some(parent))
    }

    fn cell(&mut self, limit: u128, parent: Option<u32>) -> u32 {
        self.cells.push(Cell {
            limit,
            charge: 0,
            parent,
            open: true,
            last: None,
        });
        self.cells.len() as u32 - 1
    }

    pub fn close(&mut self, cell: u32) {
        self.cells[cell as usize].open = false;
    }

    pub fn charged(&self, cell: u32) -> u128 {
        self.cells[cell as usize].charge
    }

    pub fn limit(&self, cell: u32) -> u128 {
        self.cells[cell as usize].limit
    }

    /// The cell and its ancestors, to a root cell.
    fn chain(&self, cell: u32) -> Vec<u32> {
        let mut chain = vec![cell];
        while let Some(parent) = self.cells[*chain.last().expect("a cell") as usize].parent {
            chain.push(parent);
        }
        chain
    }

    /// Draws chunks through a cell: none, and no charge, when a cell of its chain is closed or
    /// would pass its limit.
    fn draw(&mut self, cell: u32, count: u64) -> Option<Vec<u32>> {
        let bytes = u128::from(count * CHUNK);
        let chain = self.chain(cell);
        let fits = chain.iter().all(|link| {
            let link = &self.cells[*link as usize];
            link.open && link.charge + bytes <= link.limit
        });
        if !fits {
            return None;
        }
        for link in &chain {
            self.cells[*link as usize].charge += bytes;
        }
        Some(
            (0..count)
                .map(|_| {
                    self.chunks.push(Chunk {
                        cell,
                        used: 0,
                        live: 0,
                    });
                    self.chunks.len() as u32 - 1
                })
                .collect(),
        )
    }

    /// An allocation of `size` bytes through a cell: in the chunk last drawn through it when it
    /// fits after its earlier allocations, else in newly drawn chunks; none on exhaustion.
    pub fn allocate(&mut self, cell: u32, size: u64, align: u64) -> Option<u32> {
        let align = align.max(1);
        let chunks = match self.cells[cell as usize].last {
            Some(last)
                if size <= CHUNK
                    && self.chunks[last as usize].used.div_ceil(align) * align + size <= CHUNK =>
            {
                vec![last]
            }
            _ => {
                let drawn = self.draw(cell, size.div_ceil(CHUNK).max(1))?;
                if size <= CHUNK {
                    self.cells[cell as usize].last = Some(drawn[0]);
                }
                drawn
            }
        };
        let first = &mut self.chunks[chunks[0] as usize];
        first.used = first.used.div_ceil(align) * align + size.min(CHUNK);
        for chunk in &chunks {
            self.chunks[*chunk as usize].live += 1;
        }
        self.allocations.push(chunks);
        Some(self.allocations.len() as u32 - 1)
    }

    /// Frees an allocation, returning each of its chunks that holds no live allocation.
    pub fn free(&mut self, allocation: u32) {
        let chunks = std::mem::take(&mut self.allocations[allocation as usize]);
        for chunk in chunks {
            let data = &mut self.chunks[chunk as usize];
            data.live -= 1;
            if data.live > 0 {
                continue;
            }
            let cell = data.cell;
            for link in self.chain(cell) {
                let link = &mut self.cells[link as usize];
                if link.open {
                    link.charge -= u128::from(CHUNK);
                }
                if link.last == Some(chunk) {
                    link.last = None;
                }
            }
        }
    }
}
