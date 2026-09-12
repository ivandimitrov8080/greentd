//! Every place gold changes hands: what a tower costs, what selling it returns,
//! and who is paid when a creep dies.
//!
//! All three intent handlers live here together, because they are one contract:
//!
//!   1. The caller must already have a player record. Nothing here ever
//!      inserts one: a [`Player`](super::Player) is created by
//!      [`Sim::add_player`], with the table's starting gold, and by nothing
//!      else (D8).
//!   2. A tower may only be touched by its owner, and refunds go to the
//!      owner -- which, given rule 1 and the check, is also the caller (D1, D2).
//!
//! Every one of them returns `Err(`[`Reject`]`)` rather than failing silently,
//! so the server always has a reason to send back.

use bevy::prelude::IVec2;

use crate::data::reject::Reject;

use super::{PlayerKey, Sim, Tower};

impl Sim {
    /// Upgrade cost scales with the tier, so going tall is a real commitment.
    /// The curve itself lives in the tower's balance entry.
    pub fn upgrade_cost(&self, kind: u8, level: u8) -> u32 {
        match self.balance.tower(kind) {
            Some(tower) => self.balance.upgrade_cost(tower, level),
            None => 0,
        }
    }

    pub fn try_build(&mut self, who: PlayerKey, cell: IVec2, kind: u8) -> Result<(), Reject> {
        if self.over {
            return Err(Reject::MatchOver);
        }
        if !self.players.contains_key(&who) {
            return Err(Reject::NotInMatch);
        }
        let Some(tower) = self.balance.tower(kind) else {
            return Err(Reject::BadKind);
        };
        let cost = tower.cost;
        if !self.map.in_bounds(cell) {
            return Err(Reject::OutOfBounds);
        }
        if !self.is_buildable(cell) {
            return Err(Reject::PathBlocked);
        }
        if self.towers.contains_key(&cell) {
            return Err(Reject::Occupied);
        }
        let player = self.players.get_mut(&who).ok_or(Reject::NotInMatch)?;
        if player.gold < cost {
            return Err(Reject::NotEnoughGold);
        }
        player.gold -= cost;
        self.towers.insert(
            cell,
            Tower {
                kind,
                level: 1,
                owner: who,
                cooldown: 0.0,
            },
        );
        Ok(())
    }

    pub fn try_upgrade(&mut self, who: PlayerKey, cell: IVec2) -> Result<(), Reject> {
        if self.over {
            return Err(Reject::MatchOver);
        }
        if !self.players.contains_key(&who) {
            return Err(Reject::NotInMatch);
        }
        // Ownership is checked before anything is spent (D1).
        let cost = {
            let t = self.towers.get(&cell).ok_or(Reject::NoSuchTower)?;
            if t.owner != who {
                return Err(Reject::NotOwner);
            }
            let tower = self.balance.tower(t.kind).ok_or(Reject::BadKind)?;
            self.balance.upgrade_cost(tower, t.level)
        };
        let player = self.players.get_mut(&who).ok_or(Reject::NotInMatch)?;
        if player.gold < cost {
            return Err(Reject::NotEnoughGold);
        }
        player.gold -= cost;
        if let Some(t) = self.towers.get_mut(&cell) {
            t.level += 1;
        }
        Ok(())
    }

    pub fn try_sell(&mut self, who: PlayerKey, cell: IVec2) -> Result<(), Reject> {
        if !self.players.contains_key(&who) {
            return Err(Reject::NotInMatch);
        }
        let (refund, owner) = {
            let t = self.towers.get(&cell).ok_or(Reject::NoSuchTower)?;
            if t.owner != who {
                return Err(Reject::NotOwner);
            }
            // The refund fraction is `match_rules.sell_refund_percent` (D18),
            // not a literal here.
            let tower = self.balance.tower(t.kind).ok_or(Reject::BadKind)?;
            (self.balance.sell_refund(tower, t.level), t.owner)
        };
        if self.towers.remove(&cell).is_none() {
            return Err(Reject::NoSuchTower);
        }
        // The refund goes to the tower's owner, which the check above has just
        // proven is the caller (D2).
        if let Some(player) = self.players.get_mut(&owner) {
            player.gold += refund;
        }
        Ok(())
    }

    /// Pay whoever landed the last hit on a creep.
    ///
    /// A creep whose killer has since left pays nobody: the record stays (their
    /// towers do), but a disconnected player is not credited.
    pub(super) fn credit_kill(&mut self, owner: PlayerKey, bounty: u32) {
        if let Some(player) = self.players.get_mut(&owner) {
            player.gold += bounty;
            player.kills += 1;
        }
    }
}
