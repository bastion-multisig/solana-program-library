//! Program state processor

use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::Sysvar,
};
use spl_governance_addin_api::voter_weight::VoterWeightAction;
use spl_governance_tools::account::create_and_serialize_account_signed;

use crate::{
    error::GovernanceError,
    state::{
        enums::GovernanceAccountType,
        governance::get_governance_data_for_realm,
        proposal::get_proposal_data_for_governance_and_governing_mint,
        realm::get_realm_data_for_governing_token_mint,
        token_owner_record::{
            get_token_owner_record_data_for_proposal_owner,
            get_token_owner_record_data_for_realm_and_governing_mint,
        },
        vote_record::{get_vote_record_address_seeds, Vote, VoteRecordV2},
    },
};

/// Processes CastVote instruction
pub fn process_cast_vote(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    vote: Vote,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();

    let realm_info = next_account_info(account_info_iter)?; // 0
    let governance_info = next_account_info(account_info_iter)?; // 1

    let proposal_info = next_account_info(account_info_iter)?; // 2
    let proposal_owner_record_info = next_account_info(account_info_iter)?; // 3

    let voter_token_owner_record_info = next_account_info(account_info_iter)?; // 4
    let governance_authority_info = next_account_info(account_info_iter)?; // 5

    let vote_record_info = next_account_info(account_info_iter)?; // 6
    let governing_token_mint_info = next_account_info(account_info_iter)?; // 7

    let payer_info = next_account_info(account_info_iter)?; // 8
    let system_info = next_account_info(account_info_iter)?; // 9

    let rent = Rent::get()?;
    let clock = Clock::get()?;

    if !vote_record_info.data_is_empty() {
        return Err(GovernanceError::VoteAlreadyExists.into());
    }

    let mut realm_data = get_realm_data_for_governing_token_mint(
        program_id,
        realm_info,
        governing_token_mint_info.key,
    )?;
    let mut governance_data =
        get_governance_data_for_realm(program_id, governance_info, realm_info.key)?;

    let mut proposal_data = get_proposal_data_for_governance_and_governing_mint(
        program_id,
        proposal_info,
        governance_info.key,
        governing_token_mint_info.key,
    )?;
    proposal_data.assert_can_cast_vote(&governance_data.config, clock.unix_timestamp)?;

    let mut voter_token_owner_record_data =
        get_token_owner_record_data_for_realm_and_governing_mint(
            program_id,
            voter_token_owner_record_info,
            &governance_data.realm,
            governing_token_mint_info.key,
        )?;
    voter_token_owner_record_data
        .assert_token_owner_or_delegate_is_signer(governance_authority_info)?;

    // Update TokenOwnerRecord vote counts
    voter_token_owner_record_data.unrelinquished_votes_count = voter_token_owner_record_data
        .unrelinquished_votes_count
        .checked_add(1)
        .unwrap();

    voter_token_owner_record_data.total_votes_count = voter_token_owner_record_data
        .total_votes_count
        .checked_add(1)
        .unwrap();

    // Note: When both voter_weight and max_voter_weight addins are used the realm_config will be deserialized twice in resolve_voter_weight() and resolve_max_voter_weight()
    //      It can't be deserialized eagerly because some realms won't have the config if they don't use any of the advanced options
    //      This extra deserialisation should be acceptable to keep things simple and encapsulated.
    let realm_config_info = next_account_info(account_info_iter)?; //9

    let voter_weight = voter_token_owner_record_data.resolve_voter_weight(
        program_id,
        realm_config_info,
        account_info_iter, // voter_weight_record  10
        realm_info.key,
        &realm_data,
        VoterWeightAction::CastVote,
        proposal_info.key,
    )?;

    proposal_data.assert_valid_vote(&vote)?;

    // Calculate Proposal voting weights
    match &vote {
        Vote::Approve(choices) => {
            for (option, choice) in proposal_data.options.iter_mut().zip(choices) {
                option.vote_weight = option
                    .vote_weight
                    .checked_add(choice.get_choice_weight(voter_weight)?)
                    .unwrap();
            }
        }
        Vote::Deny => {
            proposal_data.deny_vote_weight = Some(
                proposal_data
                    .deny_vote_weight
                    .unwrap()
                    .checked_add(voter_weight)
                    .unwrap(),
            )
        }
        Vote::Abstain | Vote::Veto => {
            return Err(GovernanceError::NotSupportedVoteType.into());
        }
    }

    let max_voter_weight = proposal_data.resolve_max_voter_weight(
        program_id,
        realm_config_info,
        governing_token_mint_info,
        account_info_iter, // max_voter_weight_record  11
        realm_info.key,
        &realm_data,
    )?;

    if proposal_data.try_tip_vote(
        max_voter_weight,
        &governance_data.config,
        clock.unix_timestamp,
    )? {
        // Deserialize proposal owner and validate it's the actual owner of the proposal
        let mut proposal_owner_record_data = get_token_owner_record_data_for_proposal_owner(
            program_id,
            proposal_owner_record_info,
            &proposal_data.token_owner_record,
        )?;

        // If the voter is also the proposal owner then update the voter record which is serialized for the voter later on
        if proposal_owner_record_info.key == voter_token_owner_record_info.key {
            voter_token_owner_record_data.decrease_outstanding_proposal_count();
        } else {
            proposal_owner_record_data.decrease_outstanding_proposal_count();
            proposal_owner_record_data
                .serialize(&mut *proposal_owner_record_info.data.borrow_mut())?;
        };

        // Update Realm voting_proposal_count
        realm_data.voting_proposal_count = realm_data.voting_proposal_count.saturating_sub(1);
        realm_data.serialize(&mut *realm_info.data.borrow_mut())?;

        // Update  Governance voting_proposal_count
        governance_data.voting_proposal_count =
            governance_data.voting_proposal_count.saturating_sub(1);
        governance_data.serialize(&mut *governance_info.data.borrow_mut())?;
    }

    let governing_token_owner = voter_token_owner_record_data.governing_token_owner;

    voter_token_owner_record_data
        .serialize(&mut *voter_token_owner_record_info.data.borrow_mut())?;

    proposal_data.serialize(&mut *proposal_info.data.borrow_mut())?;

    // Create and serialize VoteRecord
    let vote_record_data = VoteRecordV2 {
        account_type: GovernanceAccountType::VoteRecordV2,
        proposal: *proposal_info.key,
        governing_token_owner,
        voter_weight,
        vote,
        is_relinquished: false,
        reserved_v2: [0; 8],
    };

    create_and_serialize_account_signed::<VoteRecordV2>(
        payer_info,
        vote_record_info,
        &vote_record_data,
        &get_vote_record_address_seeds(proposal_info.key, voter_token_owner_record_info.key),
        program_id,
        system_info,
        &rent,
        None,
    )?;

    Ok(())
}
