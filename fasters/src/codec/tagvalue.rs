//! FIX tag-value encoding support.
//!
//! This is the original encoding used for FIX messages and also the encoding
//! currently used by the FIX session layer.

use crate::app::{slr, TsrMessageRef, Version};
use crate::codec::{Decoder, Encoder, FramelessDecoder};
use crate::dictionary::{BaseType, Dictionary};
use crate::utils::{Buffer, BufferWriter};
use std::fmt;
use std::fmt::Debug;
use std::io;
use std::rc::Rc;
use std::str;

/// A (de)serializer for the classic FIX tag-value encoding.
///
/// The FIX tag-value encoding is designed to be both human-readable and easy for
/// machines to parse.
///
/// Please reach out to the FIX official documentation[^1][^2] for more information.
///
/// [^1]: [FIX TagValue Encoding: Online reference.](https://www.fixtrading.org/standards/tagvalue-online)
///
/// [^2]: [FIX TagValue Encoding: PDF.](https://www.fixtrading.org/standards/tagvalue/)
#[derive(Debug)]
pub struct Codec<T> {
    dict: Dictionary,
    buffer: Vec<u8>,
    state: DecoderState,
    message: T,
    body: Body,
}

impl<T> Codec<T>
where
    T: TsrMessageRef,
{
    /// Builds a new `Codec` encoding device with a FIX 4.4 dictionary.
    pub fn new() -> Self {
        Self::with_dict(Dictionary::from_version(Version::Fix44))
    }

    /// Creates a new codec for the tag-value format. `dict` is used to parse messages.
    pub fn with_dict(dict: Dictionary) -> Self {
        Self {
            dict,
            buffer: Vec::new(),
            state: DecoderState::Header,
            message: T::default(),
            body: Body::new(&[]),
        }
    }
}

#[derive(Debug)]
enum DecoderState {
    Header,
    Body(usize),
    Trailer,
}

#[derive(Debug)]
pub struct Body {
    len: usize,
    data: *const u8,
}

impl Body {
    fn new(data: &[u8]) -> Self {
        Self {
            len: data.len(),
            data: data.as_ptr(),
        }
    }
}

impl<Z> FramelessDecoder<Body> for (Codec<slr::Message>, Z)
where
    Z: Transmuter,
{
    type Error = DecodeError;

    fn supply_buffer(&mut self) -> &mut [u8] {
        let buffer_len = self.0.buffer.len();
        let additional_capacity = match self.0.state {
            DecoderState::Header => 50,
            DecoderState::Body(n) => n,
            DecoderState::Trailer => 7,
        };
        for _ in 0..additional_capacity {
            self.0.buffer.push(0);
        }
        &mut self.0.buffer[buffer_len..]
    }

    fn attempt_decoding(&mut self) -> Result<Option<&Body>, Self::Error> {
        let mut field_iter: FieldIter<_, Z> = FieldIter {
            handle: &mut &self.0.buffer[..],
            checksum: Z::ChecksumAlgo::default(),
            designator: Z::TagLookup::from_dict(&self.0.dict),
            is_last: false,
            data_length: 0,
        };
        let mut message = slr::Message::new();
        println!("begin");
        {
            // `BeginString(8)`.
            let (_, _, f) = field_iter.next().ok_or(Error::Eof)??;
            if f.tag() == 8 {
                message.set_field(f.tag() as u32, f.value().clone());
            } else {
                return Err(Error::InvalidStandardHeader);
            }
        };
        println!("beginstring done");
        {
            // `BodyLength(9)`.
            let (_, _, f) = field_iter.next().ok_or(Error::InvalidStandardHeader)??;
            if f.tag() == 9 {
                message.set_field(f.tag() as u32, f.value().clone());
            } else {
                return Err(Error::InvalidStandardHeader);
            }
        };
        println!("bodylength done");
        {
            // `MsgType(35)`.
            let (_, _, f) = field_iter.next().ok_or(Error::InvalidStandardHeader)??;
            if f.tag() == 35 {
                message.set_field(f.tag() as u32, f.value().clone());
            } else {
                return Err(Error::InvalidStandardHeader);
            }
        };
        println!("msgtype done");
        self.0.state = DecoderState::Body(0);
        self.0.state = DecoderState::Trailer;
        Ok(Some(&self.0.body))
    }

    fn get(&self) -> &Body {
        &self.0.body
    }
}

impl<Z, T> Decoder<T> for (Codec<T>, Z)
where
    T: TsrMessageRef,
    Z: Transmuter,
{
    type Error = DecodeError;

    fn decode(&mut self, mut data: &[u8]) -> Result<&T, Self::Error> {
        let mut field_iter: FieldIter<_, Z> = FieldIter {
            handle: &mut data,
            checksum: Z::ChecksumAlgo::default(),
            designator: Z::TagLookup::from_dict(&self.0.dict),
            is_last: false,
            data_length: 0,
        };
        let mut message = T::default();
        {
            // `BeginString(8)`.
            let (_, _, f) = field_iter.next().ok_or(Error::Eof)??;
            if f.tag() == 8 {
                message.set_field(f.tag() as u32, f.value().clone());
            } else {
                return Err(Error::InvalidStandardHeader);
            }
        };
        {
            // `BodyLength(9)`.
            let (_, _, f) = field_iter.next().ok_or(Error::InvalidStandardHeader)??;
            if f.tag() == 9 {
                message.set_field(f.tag() as u32, f.value().clone());
            } else {
                return Err(Error::InvalidStandardHeader);
            }
        };
        {
            // `MsgType(35)`.
            let (_, _, f) = field_iter.next().ok_or(Error::InvalidStandardHeader)??;
            if f.tag() == 35 {
                message.set_field(f.tag() as u32, f.value().clone());
            } else {
                return Err(Error::InvalidStandardHeader);
            }
        };
        let mut last_tag = 35;
        for f_result in field_iter {
            let (_, _, f) = f_result?;
            message.set_field(f.tag() as u32, f.value().clone());
            last_tag = f.tag();
        }
        if last_tag == 10 {
            self.0.message = message;
            Ok(&self.0.message)
        } else {
            Err(Error::InvalidStandardTrailer)
        }
    }
}

impl Encoder<slr::Message> for Codec<slr::Message> {
    type Error = EncodeError;

    fn encode(
        &mut self,
        mut buffer: impl Buffer,
        message: &slr::Message,
    ) -> Result<usize, Self::Error> {
        let mut writer = BufferWriter::new(&mut buffer);
        // First, write `BeginString(8)`.
        encode_field(8, message.get_field(8).unwrap(), &mut writer)?;
        // The second field is supposed to be `BodyLength(9)`, but obviously
        // the length of the message is unknow until later in the
        // serialization phase. This alone would usually require to
        //
        //  1. Serialize the rest of the message into an external buffer.
        //  2. Calculate the length of the message.
        //  3. Serialize `BodyLength(9)`.
        //  4. Copy the contents of the external buffer into `buffer`.
        //  5. ... go on with the serialization process.
        //
        // Luckily, FIX allows for zero-padded integer values and we can
        // leverage this to reserve some space for the value. We might waste
        // some bytes but the benefits largely outweight the costs.
        //
        // Six digits (~1MB) ought to be enough for every message.
        writer.extend_from_slice(b"9=000000|");
        let body_length_range = writer.len() - 7..writer.len();
        // We now must start to calculate the message length.
        let mut len = 0;
        // Third field: `MsgType(35)`.
        encode_field(35, message.get_field(35).unwrap(), &mut writer)?;
        // Now all the other fields.
        for (tag, value) in message.fields.iter() {
            if *tag != 35 {
                len += encode_field(*tag as u32, value, &mut writer)?;
            }
        }
        // Finally, we need to serialize the `Checksum(10)` field.
        //encode_field(9, &slr::FixFieldValue::Int(len as i64), &mut writer)?;
        let body_length_slice = &mut writer.as_mut_slice()[body_length_range];
        body_length_slice[3] = len as u8;
        let checksum = 42; // FIXME
        encode_field(10, &slr::FixFieldValue::Int(checksum), &mut writer)?;
        Ok(writer.len())
    }
}

fn encode_field(
    tag: u32,
    value: &slr::FixFieldValue,
    write: &mut impl io::Write,
) -> io::Result<usize> {
    let mut length = write.write(tag.to_string().as_bytes())? + 2;
    write.write_all(&[b'='])?;
    length += match &value {
        slr::FixFieldValue::Char(c) => write.write(&[*c as u8]),
        slr::FixFieldValue::String(s) => write.write(s.as_bytes()),
        slr::FixFieldValue::Int(int) => write.write(int.to_string().as_bytes()),
        slr::FixFieldValue::Float(float) => write.write(float.to_string().as_bytes()),
        slr::FixFieldValue::Data(raw_data) => write.write(&raw_data),
        slr::FixFieldValue::Group(_) => panic!("Can't encode a group!"),
    }?;
    write.write_all(&['|' as u8])?;
    Ok(length)
}

/// This trait describes dynamic tag lookup logic.
///
/// In this context, "tag lookup"
/// means to search in the dictionary the data type associated with a specific
/// tag number. This may seem trivial at best, but it can actually be quite
/// convoluted and require internal state (thus it is "dynamic" tag lookup). In
/// particular, several fields affect the internal state of a
/// [`TagLookup`](TagLookup):
///
///  - `ApplVerID <1128>`
///  - `ApplExtID <1156>`
///  - `CstmApplVerID <1129>`
///  - `DefaultApplVerID <1137>`
///  - `DefaultApplExtID <1407>`
///  - `DefaultCstmApplVerID <1408>`
///
/// Each of these fields affects the internal state and thus changes how
/// subsequent fields (and messages) are interpreted.
///
/// # Naming conventions
/// Implementors of this trait should start with `TagLookup`.
pub trait TagLookup {
    type Error: Debug;

    fn from_dict(dict: &Dictionary) -> Self;

    /// Returns the [`BaseType`] of the tag number `tag`.
    fn lookup(&mut self, tag: u32) -> Result<BaseType, Self::Error>;
}

/// A [`TagLookup`] that only allows a specific revision of the standard, like
/// most venues do.
#[derive(Debug)]
pub struct TagLookupPredetermined {
    current_dict: Rc<Dictionary>,
}

impl TagLookup for TagLookupPredetermined {
    type Error = TagLookupPredeterminedError;

    fn from_dict(dict: &Dictionary) -> Self {
        Self {
            current_dict: Rc::new(dict.clone()),
        }
    }

    fn lookup(&mut self, tag: u32) -> Result<BaseType, Self::Error> {
        // TODO
        match tag {
            // `ApplVerID <1128>`
            1128 => {}
            // `ApplExtID <1156>`
            1156 => {
                return Err(Self::Error::InvalidApplExtID);
            }
            // `CstmApplVerID <1129>`
            1129 => {
                return Err(Self::Error::InvalidCstmApplVerID);
            }
            // `DefaultApplVerID <1137>`
            1137 => {
                return Err(Self::Error::InvalidApplExtID);
            }
            // `DefaultApplExtID <1407>`
            1407 => {
                return Err(Self::Error::InvalidApplExtID);
            }
            // `DefaultCstmApplVerID <1408>`
            1408 => {
                return Err(Self::Error::InvalidCstmApplVerID);
            }
            _ => (),
        };
        Ok(self
            .current_dict
            .get_field(tag)
            .map(|f| f.basetype())
            .unwrap_or(BaseType::String))
    }
}

#[derive(Debug)]
pub enum TagLookupPredeterminedError {
    InvalidApplVerID,
    InvalidApplExtID,
    InvalidCstmApplVerID,
}

#[derive(Debug)]
pub enum TypeInfo {
    Int,
    Float,
    Char,
    String,
    Data(usize),
}

struct FieldIter<R, Z: Transmuter> {
    handle: R,
    is_last: bool,
    data_length: u32,
    checksum: Z::ChecksumAlgo,
    designator: Z::TagLookup,
}

impl<'d, R, Z> Iterator for FieldIter<&'d mut R, Z>
where
    R: io::Read,
    Z: Transmuter,
{
    type Item = Result<(u8, usize, slr::Field), DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.is_last {
            return None;
        }
        let mut buffer: Vec<u8> = Vec::new();
        let mut tag: u32 = 0;
        let mut buf = [0];
        loop {
            if self.handle.read(&mut buf).unwrap() == 0 {
                break;
            }
            let byte = buf[0];
            if byte == b'=' {
                break;
            }
            tag = tag * 10 + (byte as char).to_digit(10).unwrap();
        }
        if tag == 10 {
            self.is_last = true;
        } else if tag == 0 {
            return None;
        }
        let datatype = self.designator.lookup(tag as u32);
        match datatype {
            Ok(BaseType::Data) => {
                buffer = vec![0u8; self.data_length as usize];
                self.handle.read_exact(&mut buffer).unwrap();
                self.checksum.roll(&buffer[..]);
                self.checksum.roll(&[Z::SOH_SEPARATOR]);
                self.handle.read_exact(&mut buffer[0..1]).unwrap();
            }
            Ok(_basetype) => {
                buffer = vec![];
                loop {
                    if self.handle.read(&mut buf).unwrap() == 0 {
                        return Some(Err(Error::Eof));
                    }
                    let byte = buf[0];
                    if byte == Z::SOH_SEPARATOR {
                        break;
                    } else {
                        buffer.push(byte);
                    }
                }
                self.checksum.roll(&buffer[..]);
            }
            Err(_) => (),
        };
        let datatype = datatype.unwrap();
        let field_value = field_value(datatype, &buffer[..]).unwrap();
        if let slr::FixFieldValue::Int(l) = field_value {
            self.data_length = l as u32;
        }
        Some(Ok((
            self.checksum.clone().result(),
            self.checksum.window_length(),
            slr::Field::new(tag, field_value),
        )))
    }
}

fn field_value(datatype: BaseType, buf: &[u8]) -> Result<slr::FixFieldValue, Error> {
    Ok(match datatype {
        BaseType::Char => slr::FixFieldValue::Char(buf[0] as char),
        BaseType::String => {
            slr::FixFieldValue::String(str::from_utf8(buf).map_err(|_| Error::Syntax)?.to_string())
        }
        BaseType::Data => slr::FixFieldValue::Data(buf.to_vec()),
        BaseType::Float => slr::FixFieldValue::Float(
            str::from_utf8(buf)
                .map_err(|_| Error::Syntax)?
                .parse::<f64>()
                .map_err(|_| Error::Syntax)?,
        ),
        BaseType::Int => slr::FixFieldValue::Int(
            str::from_utf8(buf)
                .map_err(|_| Error::Syntax)?
                .parse::<i64>()
                .map_err(|_| Error::Syntax)?,
        ),
    })
}

/// The [`Transmuter`](Transmuter) pattern allows deep customization of encoding
/// and decoding behavior without relying on runtime settings. By using this
/// trait and specializing the behavior of particular methods, users can change
/// the behavior of the FIX encoder without incurring in performance loss.
///
/// # Naming conventions
/// Implementors of this trait should start with `Trans`.
pub trait Transmuter: Clone {
    type ChecksumAlgo: ChecksumAlgo;
    type TagLookup: TagLookup;

    /// The delimiter character, which terminates every tag-value pair including
    /// the last one.
    ///
    /// ASCII 0x1 is the default SOH separator character.
    const SOH_SEPARATOR: u8 = 0x1;
}

/// A [`Transmuter`] for [`Codec`] with default configuration
/// options.
///
/// This transmuter uses [`ChecksumAlgoStd`] as a checksum algorithm and
/// [`TagLookupPredetermined`] for its dynamic tag lookup logic.
#[derive(Debug, Clone)]
pub struct TransStd;

impl Transmuter for TransStd {
    type ChecksumAlgo = ChecksumAlgoStd;
    type TagLookup = TagLookupPredetermined;
}

/// A [`Transmuter`](Transmuter) for [`Codec`] with `|` (ASCII 0x7C)
/// as a field separator.
#[derive(Debug, Clone)]
pub struct TransVerticalSlash;

impl Transmuter for TransVerticalSlash {
    type ChecksumAlgo = ChecksumAlgoStd;
    type TagLookup = TagLookupPredetermined;

    const SOH_SEPARATOR: u8 = '|' as u8;
}

/// A [`Transmuter`](Transmuter) for [`Codec`] with `^` (ASCII 0x5F)
/// as a field separator.
#[derive(Debug, Clone)]
pub struct TransCaret;

impl Transmuter for TransCaret {
    type ChecksumAlgo = ChecksumAlgoStd;
    type TagLookup = TagLookupPredetermined;

    const SOH_SEPARATOR: u8 = '^' as u8;
}

/// Checksum calculation & verification algorithm. The API is designed to work
/// only with so-called "rolling" checksum algorithms, much like the one used by
/// the FIX tag-value encoding.
///
/// # Naming conventions
/// Implementors of this trait should start with `ChecksumAlgo`.
pub trait ChecksumAlgo: Default + Clone {
    /// Calculates the checksum of `window` and compounds it with `self`.
    fn roll(&mut self, window: &[u8]);

    /// Returns the amount of bytes that were processed calculating for this
    /// checksum.
    fn window_length(&self) -> usize;

    /// Returns the final checksum value.
    fn result(self) -> u8;

    /// Checks that the calculated checksum of `self` matches `checksum`.
    fn verify(&self, checksum: u8) -> bool;
}

/// A rolling checksum over a byte array. Sums over each byte wrapping around at
/// 256.
#[derive(Copy, Clone, Debug, Default)]
pub struct ChecksumAlgoStd {
    checksum: u8,
    len: usize,
}

impl ChecksumAlgo for ChecksumAlgoStd {
    fn roll(&mut self, window: &[u8]) {
        for byte in window {
            self.checksum = self.checksum.wrapping_add(*byte);
        }
        self.len += window.len();
    }

    fn window_length(&self) -> usize {
        self.len
    }

    fn result(self) -> u8 {
        self.checksum
    }

    fn verify(&self, checksum: u8) -> bool {
        self.checksum == checksum
    }
}

/// A non-verifying checksum calculator.
#[derive(Copy, Clone, Debug, Default)]
pub struct ChecksumAlgoLazy {
    len: usize,
}

impl ChecksumAlgo for ChecksumAlgoLazy {
    fn roll(&mut self, window: &[u8]) {
        self.len += window.len();
    }

    fn window_length(&self) -> usize {
        self.len
    }

    fn result(self) -> u8 {
        0
    }

    fn verify(&self, _checksum: u8) -> bool {
        true
    }
}

type DecodeError = Error;
type EncodeError = Error;

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    FieldWithoutValue(u32),
    RepeatedTag(u32),
    Eof,
    InvalidStandardHeader,
    InvalidStandardTrailer,
    InvalidChecksum(InvalidChecksum),
    Syntax,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SuperError is here!")
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl From<io::Error> for Error {
    fn from(_err: io::Error) -> Self {
        Error::Eof // FIXME
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct InvalidChecksum {
    pub expected: u8,
    pub actual: u8,
}

#[cfg(test)]
mod test {
    use super::*;

    // Use http://www.validfix.com/fix-analyzer.html for testing.

    fn encoder() -> (Codec<slr::Message>, impl Transmuter) {
        (Codec::new(), TransVerticalSlash)
    }

    #[test]
    fn can_parse_simple_message() {
        let msg = "8=FIX.4.2|9=251|35=D|49=AFUNDMGR|56=ABROKERt|15=USD|59=0|10=127|";
        let mut codec = encoder();
        let result = codec.decode(&mut msg.as_bytes());
        assert!(result.is_ok());
    }

    const RANDOM_MESSAGES: &[&str] = &[
        "8=FIX.4.2|9=42|35=0|49=A|56=B|34=12|52=20100304-07:59:30|10=185|",
        "8=FIX.4.2|9=97|35=6|49=BKR|56=IM|34=14|52=20100204-09:18:42|23=115685|28=N|55=SPMI.MI|54=2|44=2200.75|27=S|25=H|10=248|",
        "8=FIX.4.4|9=117|35=AD|34=2|49=A|50=1|52=20100219-14:33:32.258|56=B|57=M|263=1|568=1|569=0|580=1|75=20100218|60=20100218-00:00:00.000|10=202|",
        "8=FIX.4.4|9=94|35=3|34=214|49=A|50=U1|52=20100304-09:42:23.130|56=AB|128=B1|45=176|58=txt|371=15|372=X|373=1|10=058|",
        "8=FIX.4.4|9=70|35=4|49=A|56=XYZ|34=129|52=20100302-19:38:21|43=Y|57=LOL|123=Y|36=175|10=192|",
        "8=FIX.4.4|9=122|35=D|34=215|49=CLIENT12|52=20100225-19:41:57.316|56=B|1=Marcel|11=13346|21=1|40=2|44=5|54=1|59=0|60=20100225-19:39:52.020|10=072|",
        "8=FIX.4.2|9=196|35=X|49=A|56=B|34=12|52=20100318-03:21:11.364|262=A|268=2|279=0|269=0|278=BID|55=EUR/USD|270=1.37215|15=EUR|271=2500000|346=1|279=0|269=1|278=OFFER|55=EUR/USD|270=1.37224|15=EUR|271=2503200|346=1|10=171|",
        "8=FIX.4.2|9=42|35=0|49=A|56=B|34=12|52=20100304-07:59:30|10=185|",
    ];

    #[test]
    fn assortment_of_random_messages_is_ok() {
        for msg in RANDOM_MESSAGES {
            let mut codec = encoder();
            let result = codec.decode(&mut msg.as_bytes());
            assert!(result.is_ok());
        }
    }

    #[test]
    fn heartbeat_message_fields_are_ok() {
        let mut codec = encoder();
        let message = codec.decode(&mut RANDOM_MESSAGES[0].as_bytes()).unwrap();
        assert_eq!(
            message.get_field(8),
            Some(&slr::FixFieldValue::String("FIX.4.2".to_string()))
        );
        assert_eq!(message.get_field(9), Some(&slr::FixFieldValue::Int(42)));
        assert_eq!(
            message.get_field(35),
            Some(&slr::FixFieldValue::String("0".to_string()))
        );
    }

    #[test]
    fn new_order_single_without_final_separator() {
        let msg = "8=FIX.4.4|9=122|35=D|34=215|49=CLIENT12|52=20100225-19:41:57.316|56=B|1=Marcel|11=13346|21=1|40=2|44=5|54=1|59=0|60=20100225-19:39:52.020|10=072";
        let mut codec = encoder();
        let result = codec.decode(&mut msg.as_bytes());
        assert_eq!(result, Err(Error::Eof));
    }

    #[test]
    fn message_must_end_with_separator() {
        let msg = "8=FIX.4.2|9=251|35=D|49=AFUNDMGR|56=ABROKERt|15=USD|59=0|10=127";
        let mut codec = encoder();
        let result = codec.decode(&mut msg.as_bytes());
        assert_eq!(result, Err(Error::Eof));
    }

    #[test]
    fn message_without_checksum() {
        let msg = "8=FIX.4.4|9=251|35=D|49=AFUNDMGR|56=ABROKERt|15=USD|59=0|";
        let mut codec = encoder();
        let result = codec.decode(&mut msg.as_bytes());
        assert_eq!(result, Err(Error::InvalidStandardTrailer));
    }

    #[test]
    fn message_without_standard_header() {
        let msg = "35=D|49=AFUNDMGR|56=ABROKERt|15=USD|59=0|10=000|";
        let mut codec = encoder();
        let result = codec.decode(&mut msg.as_bytes());
        assert_eq!(result, Err(Error::InvalidStandardHeader));
    }

    #[test]
    fn detect_incorrect_checksum() {
        let msg = "8=FIX.4.2|9=251|35=D|49=AFUNDMGR|56=ABROKER|15=USD|59=0|10=126|";
        let _result = encoder().decode(&mut msg.as_bytes());
    }
}
