//! Merchant offers and trades: what a merchant sells, what it costs and
//! how payment slots pick an offer. What merchants offer comes from the
//! feature that owns them.

use tuff_item::item::{AsItemView, BurnReason, ItemSnapshot, ItemStack, ItemView, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::Item;

/// A player taking the results of a merchant's offers.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeRequest {
    pub merchant: i32,
    pub player: u128,
    /// Offer index and how many times it was taken, in order.
    pub trades: Vec<(usize, u32)>,
}

/// What a trading screen shows: the offers and the merchant's progress.
#[derive(Clone, Debug, PartialEq)]
pub struct MerchantView {
    pub merchant: i32,
    pub offers: Vec<MerchantOffer>,
    pub level: i32,
    pub xp: i32,
    /// A villager (level and progress bar), not a wandering trader.
    pub regular: bool,
    pub can_restock: bool,
}

/// What an offer asks for: an item, how many, and components the paid
/// stack must carry exactly (a water bottle's potion).
#[derive(Debug, Clone, PartialEq)]
pub struct ItemCost {
    pub item: Item,
    pub count: i32,
    /// The exact-match component predicate, as saved.
    pub components: Compound,
}

impl ItemCost {
    pub fn new(item: Item, count: i32) -> Self {
        Self {
            item,
            count,
            components: Compound::new(),
        }
    }

    /// The stack the cost shows: its item, count and components.
    pub fn snapshot(&self) -> ItemSnapshot {
        let mut nbt = Compound::new()
            .with("id", self.item.name())
            .with("count", self.count.clamp(1, 99));
        if !self.components.is_empty() {
            nbt.insert("components", self.components.clone());
        }
        // Mint and immediately burn into template
        let minted = ItemStack::mint_from_nbt(&nbt, MintReason::Trade)
            .unwrap_or_else(|| ItemStack::mint(self.item, 1, MintReason::Trade));
        minted.into_template()
    }

    /// Whether `stack` is this item with every required component equal.
    pub fn matches(&self, stack: ItemView) -> bool {
        if stack.item() != self.item {
            return false;
        }
        if self.components.is_empty() {
            return true;
        }
        let wanted = self.snapshot().components.to_nbt();
        let have = stack.components.to_nbt();
        self.components
            .keys()
            .all(|key| wanted.get(key).is_some() && wanted.get(key) == have.get(key))
    }

    fn to_nbt(&self) -> Compound {
        let mut nbt = Compound::new()
            .with("id", self.item.name())
            .with("count", self.count);
        if !self.components.is_empty() {
            nbt.insert("components", self.components.clone());
        }
        nbt
    }

    fn from_nbt(nbt: &Compound) -> Option<Self> {
        let item = Item::from_name(nbt.get_str("id")?)?;
        let count = match nbt.get("count") {
            Some(tag) => number(tag)? as i32,
            None => 1,
        };
        if count < 1 {
            return None;
        }
        Some(Self {
            item,
            count,
            components: nbt.get_compound("components").cloned().unwrap_or_default(),
        })
    }
}

/// One trade a merchant offers.
#[derive(Clone, Debug, PartialEq)]
pub struct MerchantOffer {
    pub buy: ItemCost,
    pub buy_b: Option<ItemCost>,
    pub sell: tuff_item::item::ItemSnapshot,
    pub uses: i32,
    pub max_uses: i32,
    pub reward_exp: bool,
    /// Added to the first cost: reputation and Hero of the Village.
    pub special_price: i32,
    pub demand: i32,
    pub price_multiplier: f32,
    pub xp: i32,
}

const DEFAULT_MAX_USES: i32 = 4;
const DEFAULT_XP: i32 = 1;

impl MerchantOffer {
    pub fn new(buy: ItemCost, buy_b: Option<ItemCost>, sell: ItemStack) -> Self {
        Self {
            buy,
            buy_b,
            sell: sell.into_template(),
            uses: 0,
            max_uses: DEFAULT_MAX_USES,
            reward_exp: true,
            special_price: 0,
            demand: 0,
            price_multiplier: 0.0,
            xp: DEFAULT_XP,
        }
    }

    /// The first cost after demand and special prices.
    pub fn cost_a(&self) -> i32 {
        let base = self.buy.count;
        let demand = ((base * self.demand) as f32 * self.price_multiplier).floor() as i32;
        let max = self.buy.item.max_stack_size() as i32;
        (base + demand.max(0) + self.special_price).clamp(1, max)
    }

    pub fn is_out_of_stock(&self) -> bool {
        self.uses >= self.max_uses
    }

    pub fn needs_restock(&self) -> bool {
        self.uses > 0
    }

    pub fn update_demand(&mut self) {
        self.demand = self.demand + self.uses - (self.max_uses - self.uses);
    }

    /// Whether the two payment stacks buy this offer, in that order.
    pub fn satisfied_by(&self, a: Option<ItemView>, b: Option<ItemView>) -> bool {
        let Some(a) = a.filter(|a| self.buy.matches(*a)) else {
            return false;
        };
        if (a.count() as i32) < self.cost_a() {
            return false;
        }
        match &self.buy_b {
            Some(cost) => b.is_some_and(|b| cost.matches(b) && b.count() as i32 >= cost.count),
            None => b.is_none(),
        }
    }

    /// Pays for the offer from the two stacks; false if they fall short.
    pub fn take(&self, a: &mut Option<ItemStack>, b: &mut Option<ItemStack>) -> bool {
        if !self.satisfied_by(a.view(), b.view()) {
            return false;
        }
        shrink(a, self.cost_a());
        if let Some(cost) = &self.buy_b {
            shrink(b, cost.count);
        }
        true
    }

    pub fn to_nbt(&self) -> Compound {
        let mut nbt = Compound::new().with("buy", self.buy.to_nbt());
        if let Some(cost) = &self.buy_b {
            nbt.insert("buyB", cost.to_nbt());
        }
        nbt.insert("sell", self.sell.to_nbt());
        if self.uses != 0 {
            nbt.insert("uses", self.uses);
        }
        if self.max_uses != DEFAULT_MAX_USES {
            nbt.insert("maxUses", self.max_uses);
        }
        if !self.reward_exp {
            nbt.insert("rewardExp", false);
        }
        if self.special_price != 0 {
            nbt.insert("specialPrice", self.special_price);
        }
        if self.demand != 0 {
            nbt.insert("demand", self.demand);
        }
        if self.price_multiplier != 0.0 {
            nbt.insert("priceMultiplier", self.price_multiplier);
        }
        if self.xp != DEFAULT_XP {
            nbt.insert("xp", self.xp);
        }
        nbt
    }

    pub fn from_nbt(nbt: &Compound) -> Option<Self> {
        let int =
            |key: &str, default: i32| nbt.get(key).and_then(number).map_or(default, |v| v as i32);
        Some(Self {
            buy: ItemCost::from_nbt(nbt.get_compound("buy")?)?,
            buy_b: nbt.get_compound("buyB").and_then(ItemCost::from_nbt),
            sell: ItemStack::from_storage(nbt.get_compound("sell")?)?.into_template(),
            uses: int("uses", 0),
            max_uses: int("maxUses", DEFAULT_MAX_USES),
            reward_exp: nbt
                .get("rewardExp")
                .and_then(number)
                .is_none_or(|v| v != 0.0),
            special_price: int("specialPrice", 0),
            demand: int("demand", 0),
            price_multiplier: nbt
                .get("priceMultiplier")
                .and_then(number)
                .map_or(0.0, |v| v as f32),
            xp: int("xp", DEFAULT_XP),
        })
    }
}

fn shrink(stack: &mut Option<ItemStack>, count: i32) {
    if let Some(s) = stack {
        let left = s.count() as i32 - count;
        if left <= 0 {
            ItemStack::burn_from(stack, u8::MAX, BurnReason::TradePayment);
        } else {
            s.shrink(count as u8, BurnReason::TradePayment);
        }
    }
}

fn number(tag: &Tag) -> Option<f64> {
    match *tag {
        Tag::Byte(v) => Some(v.into()),
        Tag::Short(v) => Some(v.into()),
        Tag::Int(v) => Some(v.into()),
        Tag::Long(v) => Some(v as f64),
        Tag::Float(v) => Some(v.into()),
        Tag::Double(v) => Some(v),
        _ => None,
    }
}

/// The saved `Offers` compound: `{Recipes: [...]}`, empty lists left out.
pub fn offers_to_nbt(offers: &[MerchantOffer]) -> Compound {
    let mut nbt = Compound::new();
    if !offers.is_empty() {
        let list: Vec<Compound> = offers.iter().map(MerchantOffer::to_nbt).collect();
        nbt.insert("Recipes", list);
    }
    nbt
}

/// Reads saved offers; entries that do not parse are dropped.
pub fn offers_from_nbt(nbt: &Compound) -> Vec<MerchantOffer> {
    nbt.get_list("Recipes")
        .unwrap_or(&[])
        .iter()
        .filter_map(|tag| MerchantOffer::from_nbt(tag.as_compound()?))
        .collect()
}

/// The offer the two payment stacks buy: the hinted one when the hint is
/// past the first offer, else the first one they satisfy.
pub fn recipe_for(
    offers: &[MerchantOffer],
    a: Option<ItemView>,
    b: Option<ItemView>,
    hint: usize,
) -> Option<usize> {
    if hint > 0 && hint < offers.len() {
        return offers[hint].satisfied_by(a, b).then_some(hint);
    }
    offers.iter().position(|offer| offer.satisfied_by(a, b))
}
