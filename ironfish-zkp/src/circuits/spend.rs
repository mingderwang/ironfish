use bellman::{Circuit, ConstraintSystem, SynthesisError};
use ff::PrimeField;

use super::util::expose_value_commitment;
use bellman::gadgets::blake2s;
use bellman::gadgets::boolean;
use bellman::gadgets::multipack;
use bellman::gadgets::num;
use bellman::gadgets::Assignment;
use zcash_primitives::{
    constants::CRH_IVK_PERSONALIZATION,
    constants::PRF_NF_PERSONALIZATION,
    sapling::{PaymentAddress, ProofGenerationKey, ValueCommitment},
};
use zcash_proofs::{
    circuit::{
        ecc::{self},
        pedersen_hash,
    },
    constants::{
        NOTE_COMMITMENT_RANDOMNESS_GENERATOR, NULLIFIER_POSITION_GENERATOR,
        PROOF_GENERATION_KEY_GENERATOR, SPENDING_KEY_GENERATOR,
    },
};

/// This is an instance of the `Spend` circuit.
pub struct Spend {
    /// Pedersen commitment to the value being spent
    pub value_commitment: Option<ValueCommitment>,

    /// Key required to construct proofs for spending notes
    /// for a particular spending key
    pub proof_generation_key: Option<ProofGenerationKey>,

    /// The payment address associated with the note
    pub payment_address: Option<PaymentAddress>,

    /// The randomness of the note commitment
    pub commitment_randomness: Option<jubjub::Fr>,

    /// Re-randomization of the public key
    pub ar: Option<jubjub::Fr>,

    /// The authentication path of the commitment in the tree
    pub auth_path: Vec<Option<(bls12_381::Scalar, bool)>>,

    /// The anchor; the root of the tree. If the note being
    /// spent is zero-value, this can be anything.
    pub anchor: Option<bls12_381::Scalar>,
}

impl Circuit<bls12_381::Scalar> for Spend {
    fn synthesize<CS: ConstraintSystem<bls12_381::Scalar>>(
        self,
        cs: &mut CS,
    ) -> Result<(), SynthesisError> {
        // Prover witnesses ak (ensures that it's on the curve)
        let ak = ecc::EdwardsPoint::witness(
            cs.namespace(|| "ak"),
            self.proof_generation_key.as_ref().map(|k| k.ak.into()),
        )?;

        // There are no sensible attacks on small order points
        // of ak (that we're aware of!) but it's a cheap check,
        // so we do it.
        ak.assert_not_small_order(cs.namespace(|| "ak not small order"))?;

        // Rerandomize ak and expose it as an input to the circuit
        {
            let ar = boolean::field_into_boolean_vec_le(cs.namespace(|| "ar"), self.ar)?;

            // Compute the randomness in the exponent
            let ar = ecc::fixed_base_multiplication(
                cs.namespace(|| "computation of randomization for the signing key"),
                &SPENDING_KEY_GENERATOR,
                &ar,
            )?;

            let rk = ak.add(cs.namespace(|| "computation of rk"), &ar)?;

            rk.inputize(cs.namespace(|| "rk"))?;
        }

        // Compute nk = [nsk] ProofGenerationKey
        let nk;
        {
            // Witness nsk as bits
            let nsk = boolean::field_into_boolean_vec_le(
                cs.namespace(|| "nsk"),
                self.proof_generation_key.as_ref().map(|k| k.nsk),
            )?;

            // NB: We don't ensure that the bit representation of nsk
            // is "in the field" (jubjub::Fr) because it's not used
            // except to demonstrate the prover knows it. If they know
            // a congruency then that's equivalent.

            // Compute nk = [nsk] ProvingPublicKey
            nk = ecc::fixed_base_multiplication(
                cs.namespace(|| "computation of nk"),
                &PROOF_GENERATION_KEY_GENERATOR,
                &nsk,
            )?;
        }

        // This is the "viewing key" preimage for CRH^ivk
        let mut ivk_preimage = vec![];

        // Place ak in the preimage for CRH^ivk
        ivk_preimage.extend(ak.repr(cs.namespace(|| "representation of ak"))?);

        // This is the nullifier preimage for PRF^nf
        let mut nf_preimage = vec![];

        // Extend ivk and nf preimages with the representation of
        // nk.
        {
            let repr_nk = nk.repr(cs.namespace(|| "representation of nk"))?;

            ivk_preimage.extend(repr_nk.iter().cloned());
            nf_preimage.extend(repr_nk);
        }

        assert_eq!(ivk_preimage.len(), 512);
        assert_eq!(nf_preimage.len(), 256);

        // Compute the incoming viewing key ivk
        let mut ivk = blake2s::blake2s(
            cs.namespace(|| "computation of ivk"),
            &ivk_preimage,
            CRH_IVK_PERSONALIZATION,
        )?;

        // drop_5 to ensure it's in the field
        ivk.truncate(jubjub::Fr::CAPACITY as usize);

        // Witness g_d, checking that it's on the curve.
        let g_d = {
            ecc::EdwardsPoint::witness(
                cs.namespace(|| "witness g_d"),
                self.payment_address
                    .as_ref()
                    .and_then(|a| a.g_d().map(jubjub::ExtendedPoint::from)),
            )?
        };

        // Check that g_d is not small order. Technically, this check
        // is already done in the Output circuit, and this proof ensures
        // g_d is bound to a product of that check, but for defense in
        // depth let's check it anyway. It's cheap.
        g_d.assert_not_small_order(cs.namespace(|| "g_d not small order"))?;

        // Compute pk_d = g_d^ivk
        let pk_d = g_d.mul(cs.namespace(|| "compute pk_d"), &ivk)?;

        // Compute note contents:
        // value (in big endian) followed by g_d and pk_d
        let mut note_contents = vec![];

        // Handle the value; we'll need it later for the
        // dummy input check.
        let mut value_num = num::Num::zero();
        {
            // Get the value in little-endian bit order
            let value_bits = expose_value_commitment(
                cs.namespace(|| "value commitment"),
                self.value_commitment,
            )?;

            // Compute the note's value as a linear combination
            // of the bits.
            let mut coeff = bls12_381::Scalar::one();
            for bit in &value_bits {
                value_num = value_num.add_bool_with_coeff(CS::one(), bit, coeff);
                coeff = coeff.double();
            }

            // Place the value in the note
            note_contents.extend(value_bits);
        }

        // Place g_d in the note
        note_contents.extend(g_d.repr(cs.namespace(|| "representation of g_d"))?);

        // Place pk_d in the note
        note_contents.extend(pk_d.repr(cs.namespace(|| "representation of pk_d"))?);

        assert_eq!(
            note_contents.len(),
            64 + // value
            256 + // g_d
            256 // p_d
        );

        // Compute the hash of the note contents
        let mut cm = pedersen_hash::pedersen_hash(
            cs.namespace(|| "note content hash"),
            pedersen_hash::Personalization::NoteCommitment,
            &note_contents,
        )?;

        {
            // Booleanize the randomness for the note commitment
            let rcm = boolean::field_into_boolean_vec_le(
                cs.namespace(|| "rcm"),
                self.commitment_randomness,
            )?;

            // Compute the note commitment randomness in the exponent
            let rcm = ecc::fixed_base_multiplication(
                cs.namespace(|| "computation of commitment randomness"),
                &NOTE_COMMITMENT_RANDOMNESS_GENERATOR,
                &rcm,
            )?;

            // Randomize the note commitment. Pedersen hashes are not
            // themselves hiding commitments.
            cm = cm.add(cs.namespace(|| "randomization of note commitment"), &rcm)?;
        }

        // This will store (least significant bit first)
        // the position of the note in the tree, for use
        // in nullifier computation.
        let mut position_bits = vec![];

        // This is an injective encoding, as cur is a
        // point in the prime order subgroup.
        let mut cur = cm.get_u().clone();

        // Ascend the merkle tree authentication path
        for (i, e) in self.auth_path.into_iter().enumerate() {
            let cs = &mut cs.namespace(|| format!("merkle tree hash {}", i));

            // Determines if the current subtree is the "right" leaf at this
            // depth of the tree.
            let cur_is_right = boolean::Boolean::from(boolean::AllocatedBit::alloc(
                cs.namespace(|| "position bit"),
                e.map(|e| e.1),
            )?);

            // Push this boolean for nullifier computation later
            position_bits.push(cur_is_right.clone());

            // Witness the authentication path element adjacent
            // at this depth.
            let path_element =
                num::AllocatedNum::alloc(cs.namespace(|| "path element"), || Ok(e.get()?.0))?;

            // Swap the two if the current subtree is on the right
            let (ul, ur) = num::AllocatedNum::conditionally_reverse(
                cs.namespace(|| "conditional reversal of preimage"),
                &cur,
                &path_element,
                &cur_is_right,
            )?;

            // We don't need to be strict, because the function is
            // collision-resistant. If the prover witnesses a congruency,
            // they will be unable to find an authentication path in the
            // tree with high probability.
            let mut preimage = vec![];
            preimage.extend(ul.to_bits_le(cs.namespace(|| "ul into bits"))?);
            preimage.extend(ur.to_bits_le(cs.namespace(|| "ur into bits"))?);

            // Compute the new subtree value
            cur = pedersen_hash::pedersen_hash(
                cs.namespace(|| "computation of pedersen hash"),
                pedersen_hash::Personalization::MerkleTree(i),
                &preimage,
            )?
            .get_u()
            .clone(); // Injective encoding
        }

        {
            let real_anchor_value = self.anchor;

            // Allocate the "real" anchor that will be exposed.
            let rt = num::AllocatedNum::alloc(cs.namespace(|| "conditional anchor"), || {
                Ok(*real_anchor_value.get()?)
            })?;

            // (cur - rt) * value = 0
            // if value is zero, cur and rt can be different
            // if value is nonzero, they must be equal
            cs.enforce(
                || "conditionally enforce correct root",
                |lc| lc + cur.get_variable() - rt.get_variable(),
                |lc| lc + &value_num.lc(bls12_381::Scalar::one()),
                |lc| lc,
            );

            // Expose the anchor
            rt.inputize(cs.namespace(|| "anchor"))?;
        }

        // Compute the cm + g^position for preventing
        // faerie gold attacks
        let mut rho = cm;
        {
            // Compute the position in the exponent
            let position = ecc::fixed_base_multiplication(
                cs.namespace(|| "g^position"),
                &NULLIFIER_POSITION_GENERATOR,
                &position_bits,
            )?;

            // Add the position to the commitment
            rho = rho.add(cs.namespace(|| "faerie gold prevention"), &position)?;
        }

        // Let's compute nf = BLAKE2s(nk || rho)
        nf_preimage.extend(rho.repr(cs.namespace(|| "representation of rho"))?);

        assert_eq!(nf_preimage.len(), 512);

        // Compute nf
        let nf = blake2s::blake2s(
            cs.namespace(|| "nf computation"),
            &nf_preimage,
            PRF_NF_PERSONALIZATION,
        )?;

        multipack::pack_into_inputs(cs.namespace(|| "pack nullifier"), &nf)
    }
}

#[cfg(test)]
mod test {
    use bellman::{
        gadgets::{multipack, test::*},
        Circuit,
    };
    use ff::{Field, PrimeField, PrimeFieldBits};
    use group::{Curve, Group};
    use rand::{RngCore, SeedableRng};
    use rand_xorshift::XorShiftRng;
    use zcash_primitives::sapling::ValueCommitment;
    use zcash_primitives::sapling::{pedersen_hash, Diversifier, Note, ProofGenerationKey, Rseed};

    use crate::circuits::spend::Spend;

    #[test]
    fn test_input_circuit_with_bls12_381() {
        let mut rng = XorShiftRng::from_seed([
            0x58, 0x62, 0xbe, 0x3d, 0x76, 0x3d, 0x31, 0x8d, 0x17, 0xdb, 0x37, 0x32, 0x54, 0x06,
            0xbc, 0xe5,
        ]);

        let tree_depth = 32;

        for _ in 0..10 {
            let value_commitment = ValueCommitment {
                value: rng.next_u64(),
                randomness: jubjub::Fr::random(&mut rng),
            };

            let proof_generation_key = ProofGenerationKey {
                ak: jubjub::SubgroupPoint::random(&mut rng),
                nsk: jubjub::Fr::random(&mut rng),
            };

            let viewing_key = proof_generation_key.to_viewing_key();

            let payment_address;

            loop {
                let diversifier = {
                    let mut d = [0; 11];
                    rng.fill_bytes(&mut d);
                    Diversifier(d)
                };

                if let Some(p) = viewing_key.to_payment_address(diversifier) {
                    payment_address = p;
                    break;
                }
            }

            let g_d = payment_address.diversifier().g_d().unwrap();
            let commitment_randomness = jubjub::Fr::random(&mut rng);
            let auth_path =
                vec![
                    Some((bls12_381::Scalar::random(&mut rng), rng.next_u32() % 2 != 0));
                    tree_depth
                ];
            let ar = jubjub::Fr::random(&mut rng);

            {
                let rk = jubjub::ExtendedPoint::from(viewing_key.rk(ar)).to_affine();
                let expected_value_commitment =
                    jubjub::ExtendedPoint::from(value_commitment.commitment()).to_affine();
                let note = Note {
                    value: value_commitment.value,
                    g_d,
                    pk_d: *payment_address.pk_d(),
                    rseed: Rseed::BeforeZip212(commitment_randomness),
                };

                let mut position = 0u64;
                let cmu = note.cmu();
                let mut cur = cmu;

                for (i, val) in auth_path.clone().into_iter().enumerate() {
                    let (uncle, b) = val.unwrap();

                    let mut lhs = cur;
                    let mut rhs = uncle;

                    if b {
                        ::std::mem::swap(&mut lhs, &mut rhs);
                    }

                    let lhs = lhs.to_le_bits();
                    let rhs = rhs.to_le_bits();

                    cur = jubjub::ExtendedPoint::from(pedersen_hash::pedersen_hash(
                        pedersen_hash::Personalization::MerkleTree(i),
                        lhs.iter()
                            .by_vals()
                            .take(bls12_381::Scalar::NUM_BITS as usize)
                            .chain(
                                rhs.iter()
                                    .by_vals()
                                    .take(bls12_381::Scalar::NUM_BITS as usize),
                            ),
                    ))
                    .to_affine()
                    .get_u();

                    if b {
                        position |= 1 << i;
                    }
                }

                let expected_nf = note.nf(&viewing_key, position);
                let expected_nf = multipack::bytes_to_bits_le(&expected_nf.0);
                let expected_nf = multipack::compute_multipacking(&expected_nf);
                assert_eq!(expected_nf.len(), 2);

                let mut cs = TestConstraintSystem::new();

                let instance = Spend {
                    value_commitment: Some(value_commitment.clone()),
                    proof_generation_key: Some(proof_generation_key.clone()),
                    payment_address: Some(payment_address.clone()),
                    commitment_randomness: Some(commitment_randomness),
                    ar: Some(ar),
                    auth_path: auth_path.clone(),
                    anchor: Some(cur),
                };

                instance.synthesize(&mut cs).unwrap();

                assert!(cs.is_satisfied());
                assert_eq!(cs.num_constraints(), 98777);
                assert_eq!(
                    cs.hash(),
                    "d37c738e83df5d9b0bb6495ac96abf21bcb2697477e2c15c2c7916ff7a3b6a89"
                );

                assert_eq!(cs.get("randomization of note commitment/u3/num"), cmu);

                assert_eq!(cs.num_inputs(), 8);
                assert_eq!(cs.get_input(0, "ONE"), bls12_381::Scalar::one());
                assert_eq!(cs.get_input(1, "rk/u/input variable"), rk.get_u());
                assert_eq!(cs.get_input(2, "rk/v/input variable"), rk.get_v());
                assert_eq!(
                    cs.get_input(3, "value commitment/commitment point/u/input variable"),
                    expected_value_commitment.get_u()
                );
                assert_eq!(
                    cs.get_input(4, "value commitment/commitment point/v/input variable"),
                    expected_value_commitment.get_v()
                );
                assert_eq!(cs.get_input(5, "anchor/input variable"), cur);
                assert_eq!(cs.get_input(6, "pack nullifier/input 0"), expected_nf[0]);
                assert_eq!(cs.get_input(7, "pack nullifier/input 1"), expected_nf[1]);
            }
        }
    }

    #[test]
    fn test_input_circuit_with_bls12_381_external_test_vectors() {
        let mut rng = XorShiftRng::from_seed([
            0x59, 0x62, 0xbe, 0x3d, 0x76, 0x3d, 0x31, 0x8d, 0x17, 0xdb, 0x37, 0x32, 0x54, 0x06,
            0xbc, 0xe5,
        ]);

        let tree_depth = 32;

        let expected_commitment_us = vec![
            "43821661663052659750276289184181083197337192946256245809816728673021647664276",
            "7220807656052227578299730541645543434083158611414003423211850718229633594616",
            "13239753550660714843257636471668037031928211668773449453628093339627668081697",
            "10900524635678389360790699587556574797582192824300145558807405770494079767974",
            "1411013767457690636461779630023011774660680126764323588543800715293173598850",
            "32334206652383066267661379202183359608706535021387905923603014648832344657662",
            "20206750741605167608500278423400565295188703622528437817438897624149653579380",
            "46716485782200334735478719487356079850582051575003452698983255860512578229998",
            "31221372899739042781372142393132358519434268512685538373976981051223051220367",
            "18269767207277008186871145355531741929166733260352590789136389380124992250945",
        ];

        let expected_commitment_vs = vec![
            "27630722367128086497290371604583225252915685718989450292520883698391703910",
            "23310648738313092772044712773481584369462075017189681529702825235349449805260",
            "25709635353183537915646348052945798827495141780341329896098121888376871589480",
            "10516315852014492141081718791576479298042117442649432716255936672048164184691",
            "23970713991179488695004801139667700217127937225554773561645815034212389459772",
            "3256052161046564597126736968199320852691566092694819239485673781545479548450",
            "18887250722195819674378865377623103071236046274361890247643850134985809137409",
            "36501156873031641173054592888886902104303750771545647842488588827138867116570",
            "21927526310070011864833939629345235038589128172309792087590183778192091594775",
            "32959334601512756708397683646222389414681003290313255304927423560477040775488",
        ];

        for i in 0..10 {
            let value_commitment = ValueCommitment {
                value: i,
                randomness: jubjub::Fr::from(1000 * (i + 1)),
            };

            let proof_generation_key = ProofGenerationKey {
                ak: jubjub::SubgroupPoint::random(&mut rng),
                nsk: jubjub::Fr::random(&mut rng),
            };

            let viewing_key = proof_generation_key.to_viewing_key();

            let payment_address;

            loop {
                let diversifier = {
                    let mut d = [0; 11];
                    rng.fill_bytes(&mut d);
                    Diversifier(d)
                };

                if let Some(p) = viewing_key.to_payment_address(diversifier) {
                    payment_address = p;
                    break;
                }
            }

            let g_d = payment_address.diversifier().g_d().unwrap();
            let commitment_randomness = jubjub::Fr::random(&mut rng);
            let auth_path =
                vec![
                    Some((bls12_381::Scalar::random(&mut rng), rng.next_u32() % 2 != 0));
                    tree_depth
                ];
            let ar = jubjub::Fr::random(&mut rng);

            {
                let rk = jubjub::ExtendedPoint::from(viewing_key.rk(ar)).to_affine();
                let expected_value_commitment =
                    jubjub::ExtendedPoint::from(value_commitment.commitment()).to_affine();
                assert_eq!(
                    expected_value_commitment.get_u(),
                    bls12_381::Scalar::from_str_vartime(expected_commitment_us[i as usize])
                        .unwrap()
                );
                assert_eq!(
                    expected_value_commitment.get_v(),
                    bls12_381::Scalar::from_str_vartime(expected_commitment_vs[i as usize])
                        .unwrap()
                );
                let note = Note {
                    value: value_commitment.value,
                    g_d,
                    pk_d: *payment_address.pk_d(),
                    rseed: Rseed::BeforeZip212(commitment_randomness),
                };

                let mut position = 0u64;
                let cmu = note.cmu();
                let mut cur = cmu;

                for (i, val) in auth_path.clone().into_iter().enumerate() {
                    let (uncle, b) = val.unwrap();

                    let mut lhs = cur;
                    let mut rhs = uncle;

                    if b {
                        ::std::mem::swap(&mut lhs, &mut rhs);
                    }

                    let lhs = lhs.to_le_bits();
                    let rhs = rhs.to_le_bits();

                    cur = jubjub::ExtendedPoint::from(pedersen_hash::pedersen_hash(
                        pedersen_hash::Personalization::MerkleTree(i),
                        lhs.iter()
                            .by_vals()
                            .take(bls12_381::Scalar::NUM_BITS as usize)
                            .chain(
                                rhs.iter()
                                    .by_vals()
                                    .take(bls12_381::Scalar::NUM_BITS as usize),
                            ),
                    ))
                    .to_affine()
                    .get_u();

                    if b {
                        position |= 1 << i;
                    }
                }

                let expected_nf = note.nf(&viewing_key, position);
                let expected_nf = multipack::bytes_to_bits_le(&expected_nf.0);
                let expected_nf = multipack::compute_multipacking(&expected_nf);
                assert_eq!(expected_nf.len(), 2);

                let mut cs = TestConstraintSystem::new();

                let instance = Spend {
                    value_commitment: Some(value_commitment.clone()),
                    proof_generation_key: Some(proof_generation_key.clone()),
                    payment_address: Some(payment_address.clone()),
                    commitment_randomness: Some(commitment_randomness),
                    ar: Some(ar),
                    auth_path: auth_path.clone(),
                    anchor: Some(cur),
                };

                instance.synthesize(&mut cs).unwrap();

                assert!(cs.is_satisfied());
                assert_eq!(cs.num_constraints(), 98777);
                assert_eq!(
                    cs.hash(),
                    "d37c738e83df5d9b0bb6495ac96abf21bcb2697477e2c15c2c7916ff7a3b6a89"
                );

                assert_eq!(cs.get("randomization of note commitment/u3/num"), cmu);

                assert_eq!(cs.num_inputs(), 8);
                assert_eq!(cs.get_input(0, "ONE"), bls12_381::Scalar::one());
                assert_eq!(cs.get_input(1, "rk/u/input variable"), rk.get_u());
                assert_eq!(cs.get_input(2, "rk/v/input variable"), rk.get_v());
                assert_eq!(
                    cs.get_input(3, "value commitment/commitment point/u/input variable"),
                    expected_value_commitment.get_u()
                );
                assert_eq!(
                    cs.get_input(4, "value commitment/commitment point/v/input variable"),
                    expected_value_commitment.get_v()
                );
                assert_eq!(cs.get_input(5, "anchor/input variable"), cur);
                assert_eq!(cs.get_input(6, "pack nullifier/input 0"), expected_nf[0]);
                assert_eq!(cs.get_input(7, "pack nullifier/input 1"), expected_nf[1]);
            }
        }
    }
}
