// std
// crates
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
// internal
use nomos_ledger::leader_proof::LeaderProof;
use nomos_proof_statements::leadership::{LeaderPrivate, LeaderPublic};

#[derive(Debug, Clone)]
pub struct Risc0LeaderProof {
    public_inputs: LeaderPublic,
    risc0_receipt: risc0_zkvm::Receipt,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Risc0 proof failed: {0}")]
    Risc0ProofFailed(#[from] anyhow::Error),
}

impl Risc0LeaderProof {
    pub fn prove(
        public_inputs: LeaderPublic,
        private_inputs: LeaderPrivate,
        prover: &dyn risc0_zkvm::Prover,
    ) -> Result<Self, Error> {
        let env = risc0_zkvm::ExecutorEnv::builder()
            .write(&public_inputs)
            .unwrap()
            .write(&private_inputs)
            .unwrap()
            .build()
            .unwrap();

        let start_t = std::time::Instant::now();

        // ATTENTION: producing a groth16 proof currently requires x86 with docker support
        let opts = risc0_zkvm::ProverOpts::groth16();
        let prove_info =
            prover.prove_with_opts(env, nomos_risc0_proofs::PROOF_OF_LEADERSHIP_ELF, &opts)?;

        tracing::debug!(
            "STARK prover time: {:.2?}, total_cycles: {}",
            start_t.elapsed(),
            prove_info.stats.total_cycles
        );
        // extract the receipt.
        Ok(Self {
            public_inputs,
            risc0_receipt: prove_info.receipt,
        })
    }
}

impl LeaderProof for Risc0LeaderProof {
    fn verify(&self, public_inputs: &LeaderPublic) -> bool {
        // The risc0 proof is valid by contract
        public_inputs == &self.public_inputs
    }

    fn nullifier(&self) -> cl::Nullifier {
        self.public_inputs.nullifier
    }

    fn evolved_commitment(&self) -> cl::NoteCommitment {
        self.public_inputs.evolved_commitment
    }

    fn merke_root(&self) -> [u8; 32] {
        self.public_inputs.cm_root
    }
}

impl Serialize for Risc0LeaderProof {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.risc0_receipt.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Risc0LeaderProof {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let risc0_receipt = risc0_zkvm::Receipt::deserialize(deserializer)?;
        let public_inputs = risc0_receipt
            .journal
            .decode()
            .map_err(|e| D::Error::custom(format!("Invalid public inputs: {e}")))?;

        risc0_receipt
            .verify(nomos_risc0_proofs::PROOF_OF_LEADERSHIP_ID)
            .map_err(D::Error::custom)?;

        Ok(Self {
            public_inputs,
            risc0_receipt,
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use cl::{note::NoteWitness, nullifier::NullifierSecret};
    use rand::thread_rng;

    const MAX_NOTE_COMMS: usize = 1 << 8;

    // derive-unit(NOMOS_NMO)
    pub const NMO_UNIT: [u8; 32] = [
        67, 50, 140, 228, 181, 204, 226, 242, 254, 193, 239, 51, 237, 68, 36, 126, 124, 227, 60,
        112, 223, 195, 146, 236, 5, 21, 42, 215, 48, 122, 25, 195,
    ];

    fn note_commitment_leaves(
        note_commitments: &[cl::NoteCommitment],
    ) -> [[u8; 32]; MAX_NOTE_COMMS] {
        let note_comm_bytes =
            Vec::from_iter(note_commitments.iter().map(|c| c.as_bytes().to_vec()));
        let cm_leaves = cl::merkle::padded_leaves::<MAX_NOTE_COMMS>(&note_comm_bytes);
        cm_leaves
    }

    #[test]
    fn test_leader_prover() {
        let mut rng = thread_rng();

        let note = NoteWitness::basic(32, NMO_UNIT, &mut rng);
        let nf_sk = NullifierSecret::random(&mut rng);

        let notes = vec![note.commit(nf_sk.commit())];
        let leaves = note_commitment_leaves(&notes);

        let input = cl::InputWitness {
            note,
            nf_sk,
            cm_path: cl::merkle::path(leaves, 0),
        };

        let epoch_nonce = [0u8; 32];
        let slot = 0;
        let active_slot_coefficient = 0.05;
        let total_stake = 1000;

        let mut expected_public_inputs = LeaderPublic::new(
            cl::merkle::root(leaves),
            epoch_nonce,
            slot,
            active_slot_coefficient,
            total_stake,
            input.nullifier(),
            input.evolve_output(b"NOMOS_POL").commit_note(),
        );

        while !expected_public_inputs.check_winning(&input) {
            expected_public_inputs.slot += 1;
        }

        println!("slot={}", expected_public_inputs.slot);

        let private_inputs = LeaderPrivate {
            input: input.clone(),
        };

        let proof = Risc0LeaderProof::prove(
            expected_public_inputs,
            private_inputs,
            risc0_zkvm::default_prover().as_ref(),
        )
        .unwrap();
        assert!(proof
            .risc0_receipt
            .verify(nomos_risc0_proofs::PROOF_OF_LEADERSHIP_ID)
            .is_ok());

        assert_eq!(
            expected_public_inputs,
            proof.risc0_receipt.journal.decode().unwrap()
        );
    }
}
