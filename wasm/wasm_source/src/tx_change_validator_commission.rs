//! A tx for a validator to change their commission rate for PoS rewards.

use namada_tx_prelude::transaction::pos::CommissionChange;
use namada_tx_prelude::*;

#[transaction]
fn apply_tx(ctx: &mut Ctx, tx_data: Vec<u8>) -> TxResult {
    let signed = SignedTxData::try_from_slice(&tx_data[..])
        .wrap_err("failed to decode SignedTxData")?;
    let data = signed.data.ok_or_err_msg("Missing data")?;
    let CommissionChange {
        validator,
        new_rate,
    } = transaction::pos::CommissionChange::try_from_slice(&data[..])
        .wrap_err("failed to decode Decimal value")?;
    ctx.change_validator_commission_rate(&validator, &new_rate)
}

#[cfg(test)]
mod tests {
    use namada::ledger::pos::PosParams;
    use namada::proto::Tx;
    use namada::types::storage::Epoch;
    use namada_tests::log::test;
    use namada_tests::native_vp::pos::init_pos;
    use namada_tests::native_vp::TestNativeVpEnv;
    use namada_tests::tx::*;
    use namada_tx_prelude::address::testing::{
        arb_established_address,
    };
    use namada_tx_prelude::key::testing::arb_common_keypair;
    use namada_tx_prelude::key::RefTo;
    use namada_tx_prelude::proof_of_stake::parameters::testing::arb_pos_params;
    use namada_tx_prelude::token;
    use namada_vp_prelude::proof_of_stake::{
        CommissionRates, GenesisValidator, PosVP,
    };
    use proptest::prelude::*;
    use rust_decimal::Decimal;

    use super::*;

    proptest! {
        /// In this test we setup the ledger and PoS system with an arbitrary
        /// initial state with 1 genesis validator and arbitrary PoS parameters. We then
        /// generate an arbitrary bond that we'd like to apply.
        ///
        /// After we apply the bond, we check that all the storage values
        /// in PoS system have been updated as expected and then we also check
        /// that this transaction is accepted by the PoS validity predicate.
        #[test]
        fn test_tx_change_validator_commissions(
            initial_rate in arb_rate(),
            max_change in arb_rate(),
            commission_change in arb_commission_change(),
            // A key to sign the transaction
            key in arb_common_keypair(),
            pos_params in arb_pos_params()) {
            test_tx_change_validator_commission_aux(commission_change, initial_rate, max_change, key, pos_params).unwrap()
        }
    }

    fn test_tx_change_validator_commission_aux(
        commission_change: transaction::pos::CommissionChange,
        initial_rate: Decimal,
        max_change: Decimal,
        key: key::common::SecretKey,
        pos_params: PosParams,
    ) -> TxResult {
        let consensus_key = key::testing::keypair_1().ref_to();
        let genesis_validators = [GenesisValidator {
            address: commission_change.validator.clone(),
            tokens: token::Amount::from(1_000_000),
            consensus_key,
            commission_rate: initial_rate,
            max_commission_rate_change: max_change,
        }];

        println!("\nInitial rate = {}\nMax change = {}\nNew rate = {}",initial_rate,max_change,commission_change.new_rate.clone());

        init_pos(&genesis_validators[..], &pos_params, Epoch(0));

        let tx_code = vec![];
        let tx_data = commission_change.try_to_vec().unwrap();
        let tx = Tx::new(tx_code, Some(tx_data));
        let signed_tx = tx.sign(&key);
        let tx_data = signed_tx.data.unwrap();

        println!("\ndbg0\n");
        // Read the data before the tx is executed
        let commission_rates_pre: CommissionRates = ctx()
            .read_validator_commission_rate(&commission_change.validator)?
            .expect("PoS validator must have commission rates");
        let commission_rate = *commission_rates_pre
            .get(0)
            .expect("PoS validator must have commission rate at genesis");
        assert_eq!(commission_rate, initial_rate);
        println!("\ndbg1\n");

        apply_tx(ctx(), tx_data)?;
        println!("\ndbg2\n");

        // Read the data after the tx is executed

        // The following storage keys should be updated:

        //     - `#{PoS}/validator/#{validator}/commission_rate`
        println!("dbg2.1");

        let commission_rates_post: CommissionRates = ctx()
            .read_validator_commission_rate(&commission_change.validator)?.unwrap();

        dbg!(&commission_rates_pre);
        dbg!(&commission_rates_post);

        // Before pipeline, the commission rates should not change
        for epoch in 0..pos_params.pipeline_len {
            assert_eq!(
                commission_rates_pre.get(epoch),
                commission_rates_post.get(epoch),
                "The commission rates before the pipeline offset must not change \
                 - checking in epoch: {epoch}"
            );
            assert_eq!(
                Some(&initial_rate),
                commission_rates_post.get(epoch),
                "The commission rates before the pipeline offset must not change \
                 - checking in epoch: {epoch}"
            );
        }
        println!("\ndbg3\n");

        // After pipeline, the commission rates should have changed
        for epoch in pos_params.pipeline_len..=pos_params.unbonding_len {
            assert_ne!(
                commission_rates_pre.get(epoch),
                commission_rates_post.get(epoch),
                "The commission rate after the pipeline offset must have changed \
                 - checking in epoch: {epoch}"
            );
            assert_eq!(
                Some(&commission_change.new_rate),
                commission_rates_post.get(epoch),
                "The commission rate after the pipeline offset must be the new_rate \
                 - checking in epoch: {epoch}"
            );
        }

        println!("\ndbg4\n");

        // Use the tx_env to run PoS VP
        let tx_env = tx_host_env::take();
        let vp_env = TestNativeVpEnv::from_tx_env(tx_env, address::POS);
        let result = vp_env.validate_tx(PosVP::new);
        let result =
            result.expect("Validation of valid changes must not fail!");
        assert!(
            result,
            "PoS Validity predicate must accept this transaction"
        );
        println!("\ndbg5\n");

        Ok(())
    }

    fn arb_rate() -> impl Strategy<Value = Decimal> {
        (0..=100_000u64).prop_map(|num| {
            Decimal::from(num) / Decimal::new(100_000, 0)
        })
    }

    fn arb_commission_change()
    -> impl Strategy<Value = transaction::pos::CommissionChange> {
        (arb_established_address(), arb_rate()).prop_map(
            |(validator, new_rate)| transaction::pos::CommissionChange {
                validator: Address::Established(validator),
                new_rate,
            },
        )
    }
}
