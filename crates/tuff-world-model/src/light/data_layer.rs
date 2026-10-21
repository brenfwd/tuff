/// Bytes in one section's nibble array.
pub const LAYER_BYTES: usize = 2048;

/// One section's light levels: 4096 nibbles indexed `y << 8 | z << 4 | x`.
/// A layer whose array was never written is homogeneous, holding `fill` everywhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataLayer {
    data: Option<Box<[u8; LAYER_BYTES]>>,
    fill: u8,
}

impl Default for DataLayer {
    fn default() -> Self {
        Self::filled(0)
    }
}

impl DataLayer {
    pub fn filled(level: u8) -> Self {
        Self {
            data: None,
            fill: level,
        }
    }

    pub fn from_bytes(data: Box<[u8; LAYER_BYTES]>) -> Self {
        Self {
            data: Some(data),
            fill: 0,
        }
    }

    fn index(x: i32, y: i32, z: i32) -> usize {
        ((y as usize) << 8) | ((z as usize) << 4) | x as usize
    }

    /// Section-relative coordinates, each in `0..16`.
    pub fn get(&self, x: i32, y: i32, z: i32) -> u8 {
        match &self.data {
            None => self.fill,
            Some(data) => {
                let index = Self::index(x, y, z);
                data[index >> 1] >> ((index & 1) * 4) & 0x0F
            }
        }
    }

    pub fn set(&mut self, x: i32, y: i32, z: i32, level: u8) {
        let index = Self::index(x, y, z);
        let shift = (index & 1) * 4;
        let byte = &mut self.data_mut()[index >> 1];
        *byte = (*byte & !(0x0F << shift)) | ((level & 0x0F) << shift);
    }

    /// Sets every cell of the horizontal slice `y` (in `0..16`) to `level`.
    pub fn fill_slice(&mut self, y: i32, level: u8) {
        let start = (y as usize) << 7;
        self.data_mut()[start..start + 128].fill(level | level << 4);
    }

    pub fn fill(&mut self, level: u8) {
        self.fill = level;
        self.data = None;
    }

    fn data_mut(&mut self) -> &mut [u8; LAYER_BYTES] {
        let fill = self.fill;
        self.data
            .get_or_insert_with(|| Box::new([fill | fill << 4; LAYER_BYTES]))
    }

    /// The nibble array, materialized if the layer is homogeneous.
    pub fn to_bytes(&self) -> Box<[u8; LAYER_BYTES]> {
        match &self.data {
            Some(data) => data.clone(),
            None => Box::new([self.fill | self.fill << 4; LAYER_BYTES]),
        }
    }

    pub fn is_definitely_homogeneous(&self) -> bool {
        self.data.is_none()
    }

    /// Only a never-written, zero-filled layer counts: an explicitly written
    /// array of zeros is not empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_none() && self.fill == 0
    }

    /// A copy whose every horizontal slice repeats this layer's bottom slice.
    pub fn repeat_first_slice(&self) -> Self {
        let Some(data) = &self.data else {
            return self.clone();
        };
        let mut out = Box::new([0u8; LAYER_BYTES]);
        let (first, _) = data.as_chunks::<128>();
        for slice in out.as_chunks_mut::<128>().0 {
            *slice = first[0];
        }
        Self::from_bytes(out)
    }
}
