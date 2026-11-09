//! Reveal transactions rebuilt with other inputs, input order or outputs, for the scenarios.

use bitcoin::{absolute::LockTime, transaction, ScriptBuf, Transaction, TxOut};

/// One reveal with the inputs and the outputs of `reveals`, in order.
pub fn merge_reveals(reveals: impl IntoIterator<Item = Transaction>) -> Transaction {
    let mut merged = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: Vec::new(),
        output: Vec::new(),
    };
    for reveal in reveals {
        merged.input.extend(reveal.input);
        merged.output.extend(reveal.output);
    }
    merged
}

/// A rebuilding of a reveal: other inputs, another input order or other outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealShape {
    AsBuilt,
    /// A reveal that carries no message merged before it
    InputBefore,
    /// A reveal that carries no message merged after it
    InputAfter,
    /// One before and one after
    InputsAround,
    /// The inputs in reverse order
    Reversed,
    /// Every output paying another script
    OtherOutput,
    /// Output 0 split in two, the second paying another script
    ExtraOutput,
}

impl RevealShape {
    pub const NAMES: [&'static str; 7] = [
        "as-built",
        "input-before",
        "input-after",
        "inputs-around",
        "reversed",
        "other-output",
        "extra-output",
    ];
}

impl std::str::FromStr for RevealShape {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "as-built" => Self::AsBuilt,
            "input-before" => Self::InputBefore,
            "input-after" => Self::InputAfter,
            "inputs-around" => Self::InputsAround,
            "reversed" => Self::Reversed,
            "other-output" => Self::OtherOutput,
            "extra-output" => Self::ExtraOutput,
            _ => {
                return Err(format!(
                    "unknown reveal shape '{s}', expected one of {}",
                    Self::NAMES.join(", ")
                ))
            }
        })
    }
}

impl std::fmt::Display for RevealShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(Self::NAMES[*self as usize])
    }
}

/// `reveal` rebuilt in `shape`. `filler()` gives a reveal that carries no message (an
/// empty-data commit-reveal pair), `other_output` the script every output pays instead.
pub fn reshape_reveal<E>(
    reveal: Transaction,
    shape: RevealShape,
    mut filler: impl FnMut() -> Result<Transaction, E>,
    other_output: &ScriptBuf,
) -> Result<Transaction, E> {
    Ok(match shape {
        RevealShape::AsBuilt => reveal,
        RevealShape::InputBefore => merge_reveals([filler()?, reveal]),
        RevealShape::InputAfter => merge_reveals([reveal, filler()?]),
        RevealShape::InputsAround => {
            let before = filler()?;
            let after = filler()?;
            merge_reveals([before, reveal, after])
        }
        RevealShape::Reversed => {
            let mut reveal = reveal;
            reveal.input.reverse();
            reveal
        }
        RevealShape::OtherOutput => {
            let mut reveal = reveal;
            for output in &mut reveal.output {
                output.script_pubkey = other_output.clone();
            }
            reveal
        }
        RevealShape::ExtraOutput => {
            let mut reveal = reveal;
            let half = reveal.output[0].value / 2;
            reveal.output[0].value -= half;
            reveal.output.push(TxOut {
                value: half,
                script_pubkey: other_output.clone(),
            });
            reveal
        }
    })
}
