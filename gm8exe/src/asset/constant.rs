use crate::asset::{PascalString, ReadPascalString, WritePascalString};
use std::io;

pub struct Constant {
    /// The asset name present in GML and the editor.
    pub name: PascalString,

    /// The GML to be evaluated into a constant value at loading time.
    ///
    /// The official runner cannot invoke user defined Script assets in this, will execute invalid memory.
    pub expression: PascalString,
}

impl Constant {
    /// Constants sit inline in the game data rather than in a compressed asset block, so they read
    /// and write as two strings and nothing else -- no version tag, no length prefix of their own.
    pub fn read(reader: &mut impl io::Read) -> io::Result<Self> {
        let name = reader.read_pas_string()?;
        let expression = reader.read_pas_string()?;
        Ok(Constant { name, expression })
    }

    pub fn write(&self, writer: &mut impl io::Write) -> io::Result<()> {
        writer.write_pas_string(&self.name)?;
        writer.write_pas_string(&self.expression)?;
        Ok(())
    }
}
