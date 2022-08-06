//! Program state processor

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    pubkey::Pubkey,
};
use spl_governance_tools::account::get_account_data;

use crate::{
    error::GovernanceError,
    state::{
        proposal::get_proposal_data_for_governance,
        proposal_transaction::{
            AccountMetaData, InstructionData, InstructionDataBrief, ProposalTransactionV2,
        },
        token_owner_record::get_token_owner_record_data_for_proposal_owner,
    },
};

/// Processes InsertTransaction instruction
pub fn process_insert_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction: InstructionDataBrief,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let governance_info = next_account_info(account_info_iter)?; // 0
    let proposal_info = next_account_info(account_info_iter)?; // 1
    let token_owner_record_info = next_account_info(account_info_iter)?; // 2
    let governance_authority_info = next_account_info(account_info_iter)?; // 3

    let proposal_transaction_info = next_account_info(account_info_iter)?; // 4

    let instruction_program_id = next_account_info(account_info_iter)?; // 8
    let instruction_keys = account_info_iter.collect::<Vec<_>>(); // 9..n

    if proposal_transaction_info.data_is_empty() {
        return Err(GovernanceError::TransactionDoesNotExists.into());
    }

    let proposal_data =
        get_proposal_data_for_governance(program_id, proposal_info, governance_info.key)?;
    proposal_data.assert_can_edit_instructions()?;

    let token_owner_record_data = get_token_owner_record_data_for_proposal_owner(
        program_id,
        token_owner_record_info,
        &proposal_data.token_owner_record,
    )?;

    token_owner_record_data.assert_token_owner_or_delegate_is_signer(governance_authority_info)?;

    let mut proposal_transaction =
        get_account_data::<ProposalTransactionV2>(program_id, proposal_transaction_info)?;

    proposal_transaction.instructions.push(InstructionData {
        program_id: instruction_program_id.key.clone(),
        accounts: instruction_keys
            .iter()
            .zip(instruction.accounts.iter())
            .map(|(account_info, account_metadata)| AccountMetaData {
                pubkey: account_info.key.clone(),
                is_signer: account_metadata.is_signer,
                is_writable: account_metadata.is_writable,
            })
            .collect::<Vec<_>>(),
        data: instruction.data,
    });

    proposal_transaction.serialize(&mut *proposal_transaction_info.data.borrow_mut())?;

    Ok(())
}
