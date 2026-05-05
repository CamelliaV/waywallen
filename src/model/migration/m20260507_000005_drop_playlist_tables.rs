use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PlaylistItem::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Playlist::Table).to_owned())
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // One-way drop. Recovery is via init_v1 if the operator
        // intentionally rolls all the way back.
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Playlist {
    Table,
}

#[derive(DeriveIden)]
enum PlaylistItem {
    Table,
}
