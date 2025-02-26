// Copyright 2024 RISC Zero, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::rc::Rc;

use anyhow::Result;
use risc0_binfmt::MemoryImage;
use risc0_circuit_rv32im::prove::{emu::testutil, hal::cpu::CpuCircuitHal};
use risc0_zkp::{
    core::{digest::Digest, hash::blake2b::Blake2bCpuHashSuite},
    hal::cpu::CpuHal,
    verify::VerificationError,
};
use risc0_zkvm_methods::{multi_test::MultiTestSpec, MULTI_TEST_ELF, MULTI_TEST_ID};
use risc0_zkvm_platform::{memory, PAGE_SIZE, WORD_SIZE};
use test_log::test;

use super::{get_prover_server, HalPair, ProverImpl};
use crate::{
    host::server::testutils,
    serde::{from_slice, to_vec},
    ExecutorEnv, ExecutorImpl, ExitCode, ProveInfo, ProverOpts, ProverServer, Receipt, ReceiptKind,
    Session, VerifierContext,
};

fn prover_opts_fast() -> ProverOpts {
    ProverOpts {
        hashfn: "sha-256".to_string(),
        prove_guest_errors: false,
        receipt_kind: ReceiptKind::Composite,
    }
}

fn prove_session_fast(session: &Session) -> Receipt {
    let prover = get_prover_server(&prover_opts_fast()).unwrap();
    prover
        .prove_session(&VerifierContext::default(), session)
        .unwrap()
        .receipt
}

fn prove_nothing(hashfn: &str) -> Result<ProveInfo> {
    let env = ExecutorEnv::builder()
        .write(&MultiTestSpec::DoNothing)
        .unwrap()
        .build()
        .unwrap();
    let opts = ProverOpts {
        hashfn: hashfn.to_string(),
        prove_guest_errors: false,
        receipt_kind: ReceiptKind::Composite,
    };
    get_prover_server(&opts).unwrap().prove(env, MULTI_TEST_ELF)
}

#[test]
fn prove_nothing_succinct() {
    let env = ExecutorEnv::builder()
        .write(&MultiTestSpec::DoNothing)
        .unwrap()
        .build()
        .unwrap();
    let opts = ProverOpts::succinct();
    get_prover_server(&opts)
        .unwrap()
        .prove(env, MULTI_TEST_ELF)
        .unwrap()
        .receipt
        .inner
        .succinct()
        .unwrap(); // ensure that we got a succinct receipt.
}

#[test]
fn hashfn_poseidon2() {
    prove_nothing("poseidon2").unwrap();
}

#[test]
fn hashfn_blake2b() {
    let hal_pair = HalPair {
        hal: Rc::new(CpuHal::new(Blake2bCpuHashSuite::new_suite())),
        circuit_hal: Rc::new(CpuCircuitHal::new()),
    };
    let env = ExecutorEnv::builder()
        .write(&MultiTestSpec::DoNothing)
        .unwrap()
        .build()
        .unwrap();
    let prover = ProverImpl::new("cpu:blake2b", hal_pair, ReceiptKind::Composite);
    prover.prove(env, MULTI_TEST_ELF).unwrap();
}

#[test]
fn receipt_serde() {
    let receipt = prove_nothing("sha-256").unwrap().receipt;
    let encoded: Vec<u32> = to_vec(&receipt).unwrap();
    let decoded: Receipt = from_slice(&encoded).unwrap();
    assert_eq!(decoded, receipt);
    decoded.verify(MULTI_TEST_ID).unwrap();
}

#[test]
fn check_image_id() {
    let receipt = prove_nothing("sha-256").unwrap().receipt;
    let mut image_id: Digest = MULTI_TEST_ID.into();
    for word in image_id.as_mut_words() {
        *word = word.wrapping_add(1);
    }
    assert_eq!(
        receipt.verify(image_id).unwrap_err(),
        VerificationError::ImageVerificationError
    );
}

#[test]
fn sha_basics() {
    fn run_sha(msg: &str) -> String {
        let env = ExecutorEnv::builder()
            .write(&MultiTestSpec::ShaDigest { data: msg.into() })
            .unwrap()
            .build()
            .unwrap();
        let mut exec = ExecutorImpl::from_elf(env, MULTI_TEST_ELF).unwrap();
        let session = exec.run().unwrap();
        let receipt = prove_session_fast(&session);
        hex::encode(Digest::try_from(receipt.journal.bytes).unwrap())
    }

    assert_eq!(
        run_sha(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        run_sha("a"),
        "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
    );
    assert_eq!(
        run_sha("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        run_sha("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

#[test]
fn sha_iter() {
    let input = MultiTestSpec::ShaDigestIter {
        data: Vec::from([0u8; 32]),
        num_iter: 1500,
    };
    let env = ExecutorEnv::builder()
        .write(&input)
        .unwrap()
        .build()
        .unwrap();
    let mut exec = ExecutorImpl::from_elf(env, MULTI_TEST_ELF).unwrap();
    let session = exec.run().unwrap();
    let receipt = prove_session_fast(&session);
    let digest = Digest::try_from(receipt.journal.bytes).unwrap();
    assert_eq!(
        hex::encode(digest),
        "9d4d1940b5c0c6d09c10add9631806f9df9467884d3e9ce4a147113e27f5c02a"
    )
}

#[test]
fn bigint_accel() {
    let cases = testutils::generate_bigint_test_cases(&mut rand::thread_rng(), 10);
    for case in cases {
        println!("Running BigInt circuit test case: {:08x?}", case);
        let input = MultiTestSpec::BigInt {
            x: case.x,
            y: case.y,
            modulus: case.modulus,
        };

        let env = ExecutorEnv::builder()
            .write(&input)
            .unwrap()
            .build()
            .unwrap();
        let mut exec = ExecutorImpl::from_elf(env, MULTI_TEST_ELF).unwrap();
        let session = exec.run().unwrap();
        let receipt = prove_session_fast(&session);
        let expected = case.expected();
        let expected: &[u8] = bytemuck::cast_slice(expected.as_slice());
        assert_eq!(receipt.journal.bytes, expected);
    }
}

#[test]
fn memory_io() {
    fn run_memio(pairs: &[(usize, usize)]) -> Result<ExitCode> {
        let input = MultiTestSpec::ReadWriteMem {
            values: pairs
                .iter()
                .cloned()
                .map(|(addr, value)| (addr as u32, value as u32))
                .collect(),
        };
        let env = ExecutorEnv::builder()
            .write(&input)
            .unwrap()
            .build()
            .unwrap();
        let mut exec = ExecutorImpl::from_elf(env, MULTI_TEST_ELF)?;
        let session = exec.run()?;
        let receipt = prove_session_fast(&session);
        receipt.verify_integrity_with_context(&VerifierContext::default())?;
        Ok(receipt.claim()?.exit_code)
    }

    // Pick a memory position in the middle of the memory space, which is unlikely
    // to be touched by either the stack or heap:
    const POS: usize = crate::align_up(
        (memory::TEXT_START + memory::STACK_TOP) as usize / 2,
        WORD_SIZE,
    );

    // Double writes are fine
    assert_eq!(
        run_memio(&[(POS, 1), (POS, 1)]).unwrap(),
        ExitCode::Halted(0)
    );

    // Writes at different addresses are fine
    assert_eq!(
        run_memio(&[(POS, 1), (POS + 4, 2)]).unwrap(),
        ExitCode::Halted(0)
    );

    // Aligned write is fine
    assert_eq!(run_memio(&[(POS, 1)]).unwrap(), ExitCode::Halted(0));

    // Unaligned write is bad
    let err = run_memio(&[(POS + 1001, 1)]).err().unwrap().to_string();
    assert!(err.contains("StoreAddressMisaligned"), "{err}");

    // Aligned read is fine
    assert_eq!(run_memio(&[(POS, 0)]).unwrap(), ExitCode::Halted(0));

    // Unaligned read is bad
    let err = run_memio(&[(POS + 1, 0)]).err().unwrap().to_string();
    assert!(err.contains("LoadAddressMisaligned"), "{err}");
}

#[test]
fn session_events() {
    use std::{cell::RefCell, rc::Rc};

    use risc0_zkvm_methods::HELLO_COMMIT_ELF;

    use crate::{Segment, SessionEvents};

    struct Logger {
        on_pre_prove_segment_flag: Rc<RefCell<bool>>,
        on_post_prove_segment_flag: Rc<RefCell<bool>>,
    }

    impl SessionEvents for Logger {
        fn on_pre_prove_segment(&self, _: &Segment) {
            self.on_pre_prove_segment_flag.replace(true);
        }

        fn on_post_prove_segment(&self, _: &Segment) {
            self.on_post_prove_segment_flag.replace(true);
        }
    }

    let mut exec = ExecutorImpl::from_elf(ExecutorEnv::default(), HELLO_COMMIT_ELF).unwrap();
    let mut session = exec.run().unwrap();
    let on_pre_prove_segment_flag = Rc::new(RefCell::new(false));
    let on_post_prove_segment_flag = Rc::new(RefCell::new(false));
    let logger = Logger {
        on_pre_prove_segment_flag: on_pre_prove_segment_flag.clone(),
        on_post_prove_segment_flag: on_post_prove_segment_flag.clone(),
    };
    session.add_hook(logger);
    prove_session_fast(&session);
    assert_eq!(session.hooks.len(), 1);
    assert_eq!(on_pre_prove_segment_flag.take(), true);
    assert_eq!(on_post_prove_segment_flag.take(), true);
}

// These tests come from:
// https://github.com/riscv-software-src/riscv-tests
// They were built using the toolchain from:
// https://github.com/risc0/toolchain/releases/tag/2022.03.25
mod riscv {
    use super::prove_session_fast;
    use crate::{ExecutorEnv, ExecutorImpl};

    fn run_test(test_name: &str) {
        use std::io::Read;

        use flate2::read::GzDecoder;
        use tar::Archive;

        let bytes = include_bytes!("../testdata/riscv-tests.tgz");
        let gz = GzDecoder::new(&bytes[..]);
        let mut tar = Archive::new(gz);
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry.path().unwrap();
            let filename = path.file_name().unwrap().to_str().unwrap();
            if filename != test_name {
                continue;
            }
            let mut elf = Vec::new();
            entry.read_to_end(&mut elf).unwrap();

            let env = ExecutorEnv::default();
            let mut exec = ExecutorImpl::from_elf(env, &elf).unwrap();
            let session = exec.run().unwrap();

            prove_session_fast(&session);
        }
    }

    macro_rules! test_case {
        ($func_name:ident) => {
            #[test_log::test]
            fn $func_name() {
                run_test(stringify!($func_name));
            }
        };
    }

    test_case!(add);
    test_case!(addi);
    test_case!(and);
    test_case!(andi);
    test_case!(auipc);
    test_case!(beq);
    test_case!(bge);
    test_case!(bgeu);
    test_case!(blt);
    test_case!(bltu);
    test_case!(bne);
    test_case!(div);
    test_case!(divu);
    test_case!(jal);
    test_case!(jalr);
    test_case!(lb);
    test_case!(lbu);
    test_case!(lh);
    test_case!(lhu);
    test_case!(lui);
    test_case!(lw);
    test_case!(mul);
    test_case!(mulh);
    test_case!(mulhsu);
    test_case!(mulhu);
    test_case!(or);
    test_case!(ori);
    test_case!(rem);
    test_case!(remu);
    test_case!(sb);
    test_case!(sh);
    test_case!(simple);
    test_case!(sll);
    test_case!(slli);
    test_case!(slt);
    test_case!(slti);
    test_case!(sltiu);
    test_case!(sltu);
    test_case!(sra);
    test_case!(srai);
    test_case!(srl);
    test_case!(srli);
    test_case!(sub);
    test_case!(sw);
    test_case!(xor);
    test_case!(xori);
}

#[test]
fn pause_resume() {
    let env = ExecutorEnv::builder()
        .write(&MultiTestSpec::PauseResume(0))
        .unwrap()
        .build()
        .unwrap();
    let mut exec = ExecutorImpl::from_elf(env, MULTI_TEST_ELF).unwrap();

    // Run until sys_pause
    let session = exec.run().unwrap();
    assert_eq!(session.segments.len(), 1);
    assert_eq!(session.exit_code, ExitCode::Paused(0));
    let receipt = prove_session_fast(&session);
    let segments = &receipt.inner.composite().unwrap().segments;
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].index, 0);

    // Run until sys_halt
    let session = exec.run().unwrap();
    assert_eq!(session.exit_code, ExitCode::Halted(0));
    prove_session_fast(&session);
}

#[test]
fn pause_exit_nonzero() {
    let user_exit_code = 1;
    let env = ExecutorEnv::builder()
        .write(&MultiTestSpec::PauseResume(user_exit_code))
        .unwrap()
        .build()
        .unwrap();
    let mut exec = ExecutorImpl::from_elf(env, MULTI_TEST_ELF).unwrap();

    // Run until sys_pause
    let session = exec.run().unwrap();
    assert_eq!(session.segments.len(), 1);
    assert_eq!(session.exit_code, ExitCode::Paused(user_exit_code as u32));
    prove_session_fast(&session);

    // Run until sys_halt
    let session = exec.run().unwrap();
    assert_eq!(session.exit_code, ExitCode::Halted(0));
    prove_session_fast(&session);
}

#[test]
fn continuation() {
    const COUNT: usize = 2; // Number of total chunks to aim for.

    let program = testutil::simple_loop();
    let image = MemoryImage::new(&program, PAGE_SIZE as u32).unwrap();

    let env = ExecutorEnv::builder()
        .segment_limit_po2(14) // 16k cycles
        .build()
        .unwrap();
    let mut exec = ExecutorImpl::new(env, image).unwrap();
    let session = exec.run().unwrap();
    let segments: Vec<_> = session
        .segments
        .iter()
        .map(|x| x.resolve().unwrap())
        .collect();
    assert_eq!(segments.len(), COUNT);

    let (final_segment, segments) = segments.split_last().unwrap();
    for segment in segments {
        assert_eq!(segment.inner.exit_code, ExitCode::SystemSplit);
    }
    assert_eq!(final_segment.inner.exit_code, ExitCode::Halted(0));

    let receipt = prove_session_fast(&session);
    for (idx, receipt) in receipt
        .inner
        .composite()
        .unwrap()
        .segments
        .iter()
        .enumerate()
    {
        assert_eq!(receipt.index, idx as u32);
    }
}

#[test]
fn sys_input() {
    use hex::FromHex;
    let digest =
        Digest::from_hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
            .unwrap();
    let spec = MultiTestSpec::SysInput(digest);
    let env = ExecutorEnv::builder()
        .input_digest(digest)
        .write(&spec)
        .unwrap()
        .build()
        .unwrap();
    let mut exec = ExecutorImpl::from_elf(env, MULTI_TEST_ELF).unwrap();
    let session = exec.run().unwrap();
    assert_eq!(session.exit_code, ExitCode::Halted(0));
    prove_session_fast(&session);
}

#[cfg(feature = "docker")]
mod docker {
    use crate::{
        get_prover_server,
        host::server::prove::{DevModeProver, ProverServer},
        ExecutorEnv, InnerReceipt, ProverOpts, Receipt, ReceiptKind,
    };
    use risc0_zkvm_methods::{multi_test::MultiTestSpec, MULTI_TEST_ELF, MULTI_TEST_ID};

    #[test]
    fn stark2snark() {
        let env = ExecutorEnv::builder()
            .write(&MultiTestSpec::DoNothing)
            .unwrap()
            .build()
            .unwrap();
        let opts = ProverOpts::compact();
        get_prover_server(&opts)
            .unwrap()
            .prove(env, MULTI_TEST_ELF)
            .unwrap()
            .receipt
            .inner
            .compact()
            .unwrap(); // ensure that we got a compact receipt.
    }

    fn test_compress(opts: ProverOpts, receipt: &Receipt) {
        let prover = get_prover_server(&opts).unwrap();
        let receipt = prover.compress(&opts, receipt).unwrap();
        match opts.receipt_kind {
            ReceiptKind::Composite => match receipt.inner {
                InnerReceipt::Composite(_)
                | InnerReceipt::Succinct(_)
                | InnerReceipt::Compact(_) => {}
                InnerReceipt::Fake { .. } => panic!("unexpected fake receipt"),
            },
            ReceiptKind::Succinct => match receipt.inner {
                InnerReceipt::Succinct(_) | InnerReceipt::Compact(_) => {}
                InnerReceipt::Composite(_) => panic!("expected receipt to be succinct or smaller"),
                InnerReceipt::Fake { .. } => panic!("unexpected fake receipt"),
            },
            ReceiptKind::Compact => match receipt.inner {
                InnerReceipt::Compact(_) => {}
                InnerReceipt::Succinct(_) | InnerReceipt::Composite(_) => {
                    panic!("expected receipt to be compact or smaller")
                }
                InnerReceipt::Fake { .. } => panic!("unexpected fake receipt"),
            },
        };
        receipt.verify(MULTI_TEST_ID).unwrap();
    }

    fn test_fake_compress(receipt: &Receipt) {
        fn ensure_fake(receipt: Receipt) {
            let InnerReceipt::Fake { claim: _ } = receipt.inner else {
                panic!("expected fake receipt");
            };
        }
        let fake = Receipt::new(
            InnerReceipt::Fake {
                claim: receipt.claim().unwrap(),
            },
            receipt.clone().journal.bytes,
        );

        let prover = DevModeProver;
        let receipt = prover.compress(&ProverOpts::composite(), &fake).unwrap();
        ensure_fake(receipt);
        let receipt = prover.compress(&ProverOpts::succinct(), &fake).unwrap();
        ensure_fake(receipt);
        let receipt = prover.compress(&ProverOpts::compact(), &fake).unwrap();
        ensure_fake(receipt);
    }

    fn generate_receipt(opts: ProverOpts) -> Receipt {
        let env = ExecutorEnv::builder()
            .write(&MultiTestSpec::DoNothing)
            .unwrap()
            .build()
            .unwrap();
        let prover = get_prover_server(&opts).unwrap();
        prover.prove(env, MULTI_TEST_ELF).unwrap().receipt
    }

    #[test]
    fn prover_stark2snark() {
        // composite receipts
        let composite_receipt = &generate_receipt(ProverOpts::composite());
        self::test_compress(ProverOpts::composite(), composite_receipt);
        self::test_compress(ProverOpts::succinct(), composite_receipt);
        self::test_compress(ProverOpts::compact(), composite_receipt);

        // succinct receipts
        let succinct_receipt = &generate_receipt(ProverOpts::succinct());
        self::test_compress(ProverOpts::composite(), succinct_receipt);
        self::test_compress(ProverOpts::succinct(), succinct_receipt);
        self::test_compress(ProverOpts::compact(), succinct_receipt);

        // compact receipts
        let compact_receipt = &generate_receipt(ProverOpts::compact());
        self::test_compress(ProverOpts::composite(), compact_receipt);
        self::test_compress(ProverOpts::succinct(), compact_receipt);
        self::test_compress(ProverOpts::compact(), compact_receipt);

        // fake receipts
        self::test_fake_compress(compact_receipt);
    }
}

mod sys_verify {
    use std::sync::OnceLock;

    use crate::ReceiptKind;
    use risc0_zkvm_methods::{
        multi_test::MultiTestSpec, HELLO_COMMIT_ELF, HELLO_COMMIT_ID, MULTI_TEST_ELF, MULTI_TEST_ID,
    };
    use test_log::test;

    use super::{get_prover_server, prover_opts_fast};
    use crate::{
        serde::to_vec, sha::Digestible, ExecutorEnv, ExecutorEnvBuilder, ExitCode, ProverOpts,
        Receipt,
    };

    fn prove_hello_commit() -> Receipt {
        get_prover_server(&prover_opts_fast())
            .unwrap()
            .prove(ExecutorEnv::default(), HELLO_COMMIT_ELF)
            .unwrap()
            .receipt
    }

    fn prove_halt(exit_code: u8) -> Receipt {
        let opts = ProverOpts {
            hashfn: "sha-256".to_string(),
            prove_guest_errors: true,
            receipt_kind: ReceiptKind::Composite,
        };

        let env = ExecutorEnvBuilder::default()
            .write(&MultiTestSpec::Halt(exit_code))
            .unwrap()
            .build()
            .unwrap();
        let halt_receipt = get_prover_server(&opts)
            .unwrap()
            .prove(env, MULTI_TEST_ELF)
            .unwrap()
            .receipt;

        // Double check that the receipt verifies with the expected image ID and exit code.
        halt_receipt
            .verify_integrity_with_context(&Default::default())
            .unwrap();
        let halt_claim = halt_receipt.claim().unwrap();
        assert_eq!(halt_claim.pre.digest(), MULTI_TEST_ID.into());
        assert_eq!(halt_claim.exit_code, ExitCode::Halted(exit_code as u32));
        halt_receipt
    }

    fn hello_commit_receipt() -> &'static Receipt {
        static ONCE: OnceLock<Receipt> = OnceLock::new();
        ONCE.get_or_init(|| prove_hello_commit())
    }

    #[test]
    fn sys_verify_1() {
        let spec = MultiTestSpec::SysVerify(vec![(
            HELLO_COMMIT_ID.into(),
            hello_commit_receipt().journal.bytes.clone(),
        )]);

        // Test that providing the proven assumption results in an unconditional
        // receipt.
        let env = ExecutorEnv::builder()
            .write(&spec)
            .unwrap()
            .add_assumption(hello_commit_receipt().clone())
            .build()
            .unwrap();
        get_prover_server(&prover_opts_fast())
            .unwrap()
            .prove(env, MULTI_TEST_ELF)
            .unwrap()
            .receipt
            .verify(MULTI_TEST_ID)
            .unwrap();
    }

    #[test]
    fn sys_verify_2() {
        let spec = MultiTestSpec::SysVerify(vec![(
            HELLO_COMMIT_ID.into(),
            hello_commit_receipt().journal.bytes.clone(),
        )]);

        // Test that proving without a provided assumption results in an execution
        // failure.
        let env = ExecutorEnv::builder()
            .write(&spec)
            .unwrap()
            .build()
            .unwrap();
        assert!(get_prover_server(&prover_opts_fast())
            .unwrap()
            .prove(env, MULTI_TEST_ELF)
            .is_err());
    }

    #[test]
    fn sys_verify_3() {
        let spec = MultiTestSpec::SysVerify(vec![(
            HELLO_COMMIT_ID.into(),
            hello_commit_receipt().journal.bytes.clone(),
        )]);

        // Test that providing an unresolved assumption results in a conditional
        // receipt.
        let env = ExecutorEnv::builder()
            .write(&spec)
            .unwrap()
            .add_assumption(hello_commit_receipt().claim().unwrap())
            .build()
            .unwrap();

        // TODO(#982) Conditional receipts currently return an error on verification.
        assert!(get_prover_server(&prover_opts_fast())
            .unwrap()
            .prove(env, MULTI_TEST_ELF)
            .is_err());

        // TODO(#982) With conditional receipts, implement the following cases.
        // verify with proven corroboration in verifier success.
        // verify with unresolved corroboration in verifier success.
        // verify with no resolution results in verifier error.
        // verify with wrong resolution results in verifier error.
    }

    #[test]
    fn sys_verify_integrity() {
        let spec = &MultiTestSpec::SysVerifyIntegrity {
            claim_words: to_vec(&hello_commit_receipt().claim().unwrap()).unwrap(),
        };

        // Test that providing the proven assumption results in an unconditional
        // receipt.
        let env = ExecutorEnv::builder()
            .write(&spec)
            .unwrap()
            .add_assumption(hello_commit_receipt().clone())
            .build()
            .unwrap();
        get_prover_server(&prover_opts_fast())
            .unwrap()
            .prove(env, MULTI_TEST_ELF)
            .unwrap()
            .receipt
            .verify(MULTI_TEST_ID)
            .unwrap();

        // Test that proving without a provided assumption results in an execution
        // failure.
        let env = ExecutorEnv::builder()
            .write(&spec)
            .unwrap()
            .build()
            .unwrap();
        assert!(get_prover_server(&prover_opts_fast())
            .unwrap()
            .prove(env, MULTI_TEST_ELF)
            .is_err());

        // Test that providing an unresolved assumption results in a conditional
        // receipt.
        let env = ExecutorEnv::builder()
            .write(&spec)
            .unwrap()
            .add_assumption(hello_commit_receipt().claim().unwrap())
            .build()
            .unwrap();
        // TODO(#982) Conditional receipts currently return an error on verification.
        assert!(get_prover_server(&prover_opts_fast())
            .unwrap()
            .prove(env, MULTI_TEST_ELF)
            .is_err());
    }

    #[test]
    fn sys_verify_integrity_halt_1() {
        // Generate a receipt for a execution ending in a guest error indicated by
        // ExitCode::Halted(1).
        let halt_receipt = prove_halt(1);

        let spec = &MultiTestSpec::SysVerifyIntegrity {
            claim_words: to_vec(&halt_receipt.claim().unwrap()).unwrap(),
        };

        // Test that proving results in a success execution and unconditional receipt.
        let env = ExecutorEnv::builder()
            .write(&spec)
            .unwrap()
            .add_assumption(halt_receipt)
            .build()
            .unwrap();
        get_prover_server(&prover_opts_fast())
            .unwrap()
            .prove(env, MULTI_TEST_ELF)
            .unwrap()
            .receipt
            .verify(MULTI_TEST_ID)
            .unwrap();
    }
}

mod soundness {
    use risc0_circuit_rv32im::{prove::emu::exec::DEFAULT_SEGMENT_LIMIT_PO2, CIRCUIT};
    use risc0_zkp::{
        adapter::TapsProvider,
        field::{
            baby_bear::{BabyBear, BabyBearExtElem},
            ExtElem,
        },
        hal::cpu::CpuHal,
        prove::soundness,
    };

    #[test]
    fn proven() {
        let cycles = 1 << DEFAULT_SEGMENT_LIMIT_PO2;
        let ext_size = BabyBearExtElem::EXT_SIZE;
        let coeffs_size = cycles * ext_size;
        let taps = CIRCUIT.get_taps();

        let security = soundness::proven::<CpuHal<BabyBear>>(taps, coeffs_size);
        assert_eq!(security, 41.757866);
    }

    #[test]
    fn conjectured_strict() {
        let cycles = 1 << DEFAULT_SEGMENT_LIMIT_PO2;
        let ext_size = BabyBearExtElem::EXT_SIZE;
        let coeffs_size = cycles * ext_size;
        let taps = CIRCUIT.get_taps();

        let security = soundness::conjectured_strict::<CpuHal<BabyBear>>(taps, coeffs_size);
        assert_eq!(security, 74.90123);
    }

    #[test]
    fn toy_model() {
        let cycles: usize = 1 << DEFAULT_SEGMENT_LIMIT_PO2;
        let ext_size = BabyBearExtElem::EXT_SIZE;
        let coeffs_size = cycles * ext_size;
        let taps = CIRCUIT.get_taps();

        let security = soundness::toy_model_security::<CpuHal<BabyBear>>(taps, coeffs_size);
        assert_eq!(security, 98.32892);
    }
}
